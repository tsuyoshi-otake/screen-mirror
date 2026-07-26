package com.screenmirror;

import android.media.AudioAttributes;
import android.media.AudioFormat;
import android.media.AudioManager;
import android.media.AudioTrack;
import android.media.MediaCodec;
import android.media.MediaFormat;

import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.InetSocketAddress;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.concurrent.atomic.AtomicBoolean;

final class RtpOpusReceiver {
    static final int DEFAULT_AUDIO_PORT = 5005;

    private static final int SAMPLE_RATE = 48_000;
    private static final int CHANNELS = 2;
    private static final int SOCKET_BUFFER_SIZE = 512 * 1024;
    private static final int DSCP_EF_TRAFFIC_CLASS = 0xB8;
    private static final int RTP_HEADER_SIZE = 12;

    private final AtomicBoolean running = new AtomicBoolean(false);
    private final byte[] receiveBuffer = new byte[1500];
    private final DatagramPacket receivePacket = new DatagramPacket(receiveBuffer, receiveBuffer.length);
    private final MediaCodec.BufferInfo bufferInfo = new MediaCodec.BufferInfo();

    private Thread thread;
    private MediaCodec decoder;
    private AudioTrack audioTrack;
    private int baseTimestamp = -1;

    void start(int port) throws Exception {
        stop();
        decoder = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_AUDIO_OPUS);
        MediaFormat format = MediaFormat.createAudioFormat(MediaFormat.MIMETYPE_AUDIO_OPUS, SAMPLE_RATE, CHANNELS);
        format.setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, 1275);
        addOpusCodecSpecificData(format);
        decoder.configure(format, null, null, 0);
        decoder.start();

        int minBuffer = AudioTrack.getMinBufferSize(
                SAMPLE_RATE,
                AudioFormat.CHANNEL_OUT_STEREO,
                AudioFormat.ENCODING_PCM_16BIT
        );
        int targetBuffer = Math.max(minBuffer, SAMPLE_RATE * CHANNELS * 2 / 50);
        if (android.os.Build.VERSION.SDK_INT >= 26) {
            audioTrack = new AudioTrack.Builder()
                    .setAudioAttributes(new AudioAttributes.Builder()
                            .setUsage(AudioAttributes.USAGE_MEDIA)
                            .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                            .build())
                    .setAudioFormat(new AudioFormat.Builder()
                            .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                            .setSampleRate(SAMPLE_RATE)
                            .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO)
                            .build())
                    .setTransferMode(AudioTrack.MODE_STREAM)
                    .setBufferSizeInBytes(targetBuffer)
                    .build();
        } else {
            audioTrack = new AudioTrack(
                    AudioManager.STREAM_MUSIC,
                    SAMPLE_RATE,
                    AudioFormat.CHANNEL_OUT_STEREO,
                    AudioFormat.ENCODING_PCM_16BIT,
                    targetBuffer,
                    AudioTrack.MODE_STREAM
            );
        }
        audioTrack.play();

        baseTimestamp = -1;
        running.set(true);
        thread = new Thread(() -> receiveLoop(port), "rtp-opus-receiver");
        thread.start();
    }

    void stop() {
        running.set(false);
        if (thread != null) {
            thread.interrupt();
            thread = null;
        }
        if (decoder != null) {
            try {
                decoder.stop();
            } catch (Exception ignored) {
            }
            decoder.release();
            decoder = null;
        }
        if (audioTrack != null) {
            try {
                audioTrack.pause();
                audioTrack.flush();
            } catch (Exception ignored) {
            }
            audioTrack.release();
            audioTrack = null;
        }
        baseTimestamp = -1;
    }

    private void receiveLoop(int port) {
        try (DatagramSocket socket = new DatagramSocket(null)) {
            socket.setReuseAddress(true);
            socket.setReceiveBufferSize(SOCKET_BUFFER_SIZE);
            try {
                socket.setTrafficClass(DSCP_EF_TRAFFIC_CLASS);
            } catch (Exception ignored) {
            }
            socket.bind(new InetSocketAddress(port));
            while (running.get()) {
                receivePacket.setData(receiveBuffer, 0, receiveBuffer.length);
                socket.receive(receivePacket);
                queuePacket(receiveBuffer, receivePacket.getLength());
                drainDecoder();
            }
        } catch (Exception ignored) {
        }
    }

    private void queuePacket(byte[] packet, int length) throws Exception {
        MediaCodec activeDecoder = decoder;
        if (activeDecoder == null || length <= RTP_HEADER_SIZE) {
            return;
        }

        int payloadOffset = payloadOffset(packet, length);
        if (payloadOffset <= 0 || payloadOffset >= length) {
            return;
        }

        int payloadLength = length - payloadOffset;
        int timestamp = readInt(packet, 4);
        if (baseTimestamp < 0) {
            baseTimestamp = timestamp;
        }
        long presentationTimeUs = (((long) (timestamp - baseTimestamp)) & 0xffff_ffffL) * 1_000_000L / SAMPLE_RATE;

        int inputIndex = activeDecoder.dequeueInputBuffer(1_000);
        if (inputIndex < 0) {
            return;
        }
        ByteBuffer input = activeDecoder.getInputBuffer(inputIndex);
        if (input == null || input.capacity() < payloadLength) {
            return;
        }
        input.clear();
        input.put(packet, payloadOffset, payloadLength);
        activeDecoder.queueInputBuffer(inputIndex, 0, payloadLength, presentationTimeUs, 0);
    }

    private void drainDecoder() {
        MediaCodec activeDecoder = decoder;
        AudioTrack activeTrack = audioTrack;
        if (activeDecoder == null || activeTrack == null) {
            return;
        }

        int output;
        do {
            output = activeDecoder.dequeueOutputBuffer(bufferInfo, 0);
            if (output >= 0) {
                ByteBuffer buffer = activeDecoder.getOutputBuffer(output);
                if (buffer != null && bufferInfo.size > 0) {
                    buffer.position(bufferInfo.offset);
                    buffer.limit(bufferInfo.offset + bufferInfo.size);
                    activeTrack.write(buffer, bufferInfo.size, AudioTrack.WRITE_NON_BLOCKING);
                }
                activeDecoder.releaseOutputBuffer(output, false);
            }
        } while (output >= 0);
    }

    private static int payloadOffset(byte[] packet, int length) {
        int csrcCount = packet[0] & 0x0f;
        int offset = RTP_HEADER_SIZE + csrcCount * 4;
        if (offset >= length) {
            return -1;
        }
        boolean hasExtension = (packet[0] & 0x10) != 0;
        if (hasExtension) {
            if (offset + 4 > length) {
                return -1;
            }
            int extensionWords = ((packet[offset + 2] & 0xff) << 8) | (packet[offset + 3] & 0xff);
            offset += 4 + extensionWords * 4;
        }
        return offset;
    }

    private static int readInt(byte[] packet, int offset) {
        return ((packet[offset] & 0xff) << 24)
                | ((packet[offset + 1] & 0xff) << 16)
                | ((packet[offset + 2] & 0xff) << 8)
                | (packet[offset + 3] & 0xff);
    }

    private static void addOpusCodecSpecificData(MediaFormat format) {
        ByteBuffer opusHead = ByteBuffer.allocate(19).order(ByteOrder.LITTLE_ENDIAN);
        opusHead.put(new byte[]{'O', 'p', 'u', 's', 'H', 'e', 'a', 'd'});
        opusHead.put((byte) 1);
        opusHead.put((byte) CHANNELS);
        opusHead.putShort((short) 312);
        opusHead.putInt(SAMPLE_RATE);
        opusHead.putShort((short) 0);
        opusHead.put((byte) 0);
        opusHead.flip();
        format.setByteBuffer("csd-0", opusHead);
        format.setByteBuffer("csd-1", longBuffer(6_500_000L));
        format.setByteBuffer("csd-2", longBuffer(80_000_000L));
    }

    private static ByteBuffer longBuffer(long value) {
        ByteBuffer buffer = ByteBuffer.allocate(8).order(ByteOrder.nativeOrder());
        buffer.putLong(value);
        buffer.flip();
        return buffer;
    }
}
