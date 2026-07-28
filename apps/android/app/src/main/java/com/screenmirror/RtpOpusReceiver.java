package com.screenmirror;

import android.media.AudioAttributes;
import android.media.AudioFormat;
import android.media.AudioTrack;
import android.media.MediaCodec;
import android.media.MediaFormat;
import android.media.audiofx.LoudnessEnhancer;

import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.InetSocketAddress;
import java.net.SocketException;
import java.net.SocketTimeoutException;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.concurrent.atomic.AtomicBoolean;

final class RtpOpusReceiver {
    interface Listener {
        void onError(Throwable error);
    }

    static final int DEFAULT_AUDIO_PORT = 5005;

    private static final int SAMPLE_RATE = 48_000;
    private static final int CHANNELS = 2;
    private static final int SOCKET_BUFFER_SIZE = 256 * 1024;
    private static final int SOCKET_TIMEOUT_MS = 250;
    private static final int DSCP_EF_TRAFFIC_CLASS = 0xB8;
    private static final int RTP_HEADER_SIZE = 12;
    private static final int PCM_BYTES_PER_FRAME = CHANNELS * 2;
    static final int LOW_LATENCY_BUFFER_MS = 10;
    private static final int BUFFER_CAPACITY_MS = 20;
    private static final int BUFFER_GROWTH_MS = 5;
    static final float MAX_GAIN = 4.0f;

    private final AtomicBoolean running = new AtomicBoolean(false);
    private final byte[] receiveBuffer = new byte[1500];
    private final DatagramPacket receivePacket = new DatagramPacket(receiveBuffer, receiveBuffer.length);
    private final MediaCodec.BufferInfo bufferInfo = new MediaCodec.BufferInfo();

    private volatile Listener listener;
    private volatile DatagramSocket socket;
    private Thread thread;
    private MediaCodec decoder;
    private AudioTrack audioTrack;
    private LoudnessEnhancer loudnessEnhancer;
    private volatile float gain = 1.0f;
    private int baseTimestamp = -1;
    private int outputBufferFrames;
    private int lastUnderrunCount;
    private boolean audioTrackStarted;

    void setListener(Listener listener) {
        this.listener = listener;
    }

    /**
     * Sets the playback gain, where 1.0 is the stream as sent. Values above 1.0 are amplified with
     * a {@link LoudnessEnhancer} because {@link AudioTrack#setVolume} cannot exceed unity. The gain
     * survives stop/start so the receiver keeps the level the user picked.
     */
    synchronized void setGain(float requestedGain) {
        gain = clampGain(requestedGain);
        applyGain();
    }

    float gain() {
        return gain;
    }

    synchronized void start(int port) throws Exception {
        stop();

        DatagramSocket localSocket = createSocket(port);
        MediaCodec localDecoder = null;
        AudioTrack localTrack = null;
        int localOutputBufferFrames = 0;
        try {
            localDecoder = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_AUDIO_OPUS);
            MediaFormat format = MediaFormat.createAudioFormat(MediaFormat.MIMETYPE_AUDIO_OPUS, SAMPLE_RATE, CHANNELS);
            format.setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, 1275);
            addOpusCodecSpecificData(format);
            localDecoder.configure(format, null, null, 0);
            localDecoder.start();

            int minBuffer = AudioTrack.getMinBufferSize(
                    SAMPLE_RATE,
                    AudioFormat.CHANNEL_OUT_STEREO,
                    AudioFormat.ENCODING_PCM_16BIT
            );
            if (minBuffer <= 0) {
                throw new IllegalStateException("AudioTrack minimum buffer query failed with code " + minBuffer);
            }
            int capacityBuffer = Math.max(minBuffer, audioBytesForMilliseconds(BUFFER_CAPACITY_MS));
            localTrack = new AudioTrack.Builder()
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
                    .setPerformanceMode(AudioTrack.PERFORMANCE_MODE_LOW_LATENCY)
                    .setBufferSizeInBytes(capacityBuffer)
                    .build();
            if (localTrack.getState() != AudioTrack.STATE_INITIALIZED) {
                throw new IllegalStateException("AudioTrack did not initialize");
            }
            int requestedFrames = audioFramesForMilliseconds(LOW_LATENCY_BUFFER_MS);
            int configuredFrames = localTrack.setBufferSizeInFrames(requestedFrames);
            if (configuredFrames < 0) {
                int errorCode = configuredFrames;
                configuredFrames = localTrack.getBufferSizeInFrames();
                AppLog.warn(
                        "audio output rejected the 10 ms buffer request with code "
                                + errorCode,
                        null
                );
            }
            localOutputBufferFrames = configuredFrames;
        } catch (Exception error) {
            localSocket.close();
            if (localTrack != null) {
                localTrack.release();
            }
            if (localDecoder != null) {
                try {
                    localDecoder.stop();
                } catch (Exception stopError) {
                    AppLog.warn("audio decoder cleanup failed", stopError);
                }
                localDecoder.release();
            }
            throw error;
        }

        decoder = localDecoder;
        audioTrack = localTrack;
        loudnessEnhancer = createLoudnessEnhancer(localTrack);
        applyGain();
        socket = localSocket;
        baseTimestamp = -1;
        outputBufferFrames = localOutputBufferFrames;
        lastUnderrunCount = 0;
        audioTrackStarted = false;
        running.set(true);
        thread = new Thread(() -> receiveLoop(localSocket), "rtp-opus-receiver");
        thread.start();
        AppLog.info(
                "audio receiver started on UDP "
                        + port
                        + " with "
                        + framesToMilliseconds(localOutputBufferFrames)
                        + " ms output buffer"
        );
    }

    synchronized void stop() {
        running.set(false);
        DatagramSocket activeSocket = socket;
        socket = null;
        if (activeSocket != null) {
            activeSocket.close();
        }

        Thread worker = thread;
        thread = null;
        joinWorker(worker);

        MediaCodec activeDecoder = decoder;
        decoder = null;
        if (activeDecoder != null) {
            try {
                activeDecoder.stop();
            } catch (Exception error) {
                AppLog.warn("audio decoder stop failed", error);
            }
            activeDecoder.release();
        }

        LoudnessEnhancer activeEnhancer = loudnessEnhancer;
        loudnessEnhancer = null;
        if (activeEnhancer != null) {
            try {
                activeEnhancer.setEnabled(false);
            } catch (RuntimeException error) {
                AppLog.warn("audio gain effect disable failed", error);
            }
            activeEnhancer.release();
        }

        AudioTrack activeTrack = audioTrack;
        audioTrack = null;
        if (activeTrack != null) {
            try {
                activeTrack.pause();
                activeTrack.flush();
            } catch (Exception error) {
                AppLog.warn("audio output stop failed", error);
            }
            activeTrack.release();
        }
        baseTimestamp = -1;
        outputBufferFrames = 0;
        lastUnderrunCount = 0;
        audioTrackStarted = false;
    }

    boolean isRunning() {
        return running.get();
    }

    private static DatagramSocket createSocket(int port) throws Exception {
        DatagramSocket localSocket = new DatagramSocket(null);
        try {
            localSocket.setReuseAddress(true);
            localSocket.setReceiveBufferSize(SOCKET_BUFFER_SIZE);
            localSocket.setSoTimeout(SOCKET_TIMEOUT_MS);
            try {
                localSocket.setTrafficClass(DSCP_EF_TRAFFIC_CLASS);
            } catch (SocketException error) {
                AppLog.warn("audio receiver could not set DSCP", error);
            }
            localSocket.bind(new InetSocketAddress(port));
            return localSocket;
        } catch (Exception error) {
            localSocket.close();
            throw error;
        }
    }

    private void receiveLoop(DatagramSocket localSocket) {
        try {
            while (running.get()) {
                try {
                    receivePacket.setData(receiveBuffer, 0, receiveBuffer.length);
                    localSocket.receive(receivePacket);
                    queuePacket(receiveBuffer, receivePacket.getLength());
                    drainDecoder();
                } catch (SocketTimeoutException timeout) {
                    // A short timeout lets stop() close and join this worker promptly.
                } catch (SocketException error) {
                    if (running.get()) {
                        fail(error);
                    }
                }
            }
        } catch (Exception error) {
            if (running.get()) {
                fail(error);
            }
        } finally {
            localSocket.close();
            if (socket == localSocket) {
                socket = null;
            }
        }
    }

    private void fail(Throwable error) {
        running.set(false);
        AppLog.error("audio receiver failed", error);
        Listener activeListener = listener;
        if (activeListener != null) {
            activeListener.onError(error);
        }
    }

    private void queuePacket(byte[] packet, int length) throws Exception {
        MediaCodec activeDecoder = decoder;
        if (activeDecoder == null || length <= RTP_HEADER_SIZE || (packet[0] & 0xc0) != 0x80) {
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
            activeDecoder.queueInputBuffer(inputIndex, 0, 0, presentationTimeUs, 0);
            AppLog.warn("audio decoder input buffer was too small", null);
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
                try {
                    ByteBuffer buffer = activeDecoder.getOutputBuffer(output);
                    if (buffer != null && bufferInfo.size > 0) {
                        buffer.position(bufferInfo.offset);
                        buffer.limit(bufferInfo.offset + bufferInfo.size);
                        boolean starting = !audioTrackStarted;
                        if (starting) {
                            activeTrack.play();
                            audioTrackStarted = true;
                        }
                        int written = activeTrack.write(buffer, bufferInfo.size, AudioTrack.WRITE_BLOCKING);
                        if (written < 0) {
                            throw new IllegalStateException("AudioTrack write failed with code " + written);
                        }
                        if (starting) {
                            lastUnderrunCount = activeTrack.getUnderrunCount();
                        } else {
                            adaptOutputBuffer(activeTrack);
                        }
                    }
                } finally {
                    activeDecoder.releaseOutputBuffer(output, false);
                }
            }
        } while (output >= 0);
    }

    private void adaptOutputBuffer(AudioTrack activeTrack) {
        if (!audioTrackStarted) {
            return;
        }
        int underrunCount = activeTrack.getUnderrunCount();
        if (underrunCount <= lastUnderrunCount) {
            return;
        }
        lastUnderrunCount = underrunCount;

        int capacityFrames = activeTrack.getBufferCapacityInFrames();
        int requestedFrames = Math.min(
                capacityFrames,
                outputBufferFrames + audioFramesForMilliseconds(BUFFER_GROWTH_MS)
        );
        if (requestedFrames <= outputBufferFrames) {
            return;
        }
        int configuredFrames = activeTrack.setBufferSizeInFrames(requestedFrames);
        if (configuredFrames > outputBufferFrames) {
            outputBufferFrames = configuredFrames;
            AppLog.warn(
                    "audio output underrun; expanded buffer to "
                            + framesToMilliseconds(configuredFrames)
                            + " ms",
                    null
            );
        }
    }

    private static LoudnessEnhancer createLoudnessEnhancer(AudioTrack track) {
        try {
            return new LoudnessEnhancer(track.getAudioSessionId());
        } catch (RuntimeException error) {
            // Some devices ship without the effect; playback still works up to unity gain.
            AppLog.warn("audio gain effect unavailable; boost above 100% is disabled", error);
            return null;
        }
    }

    private void applyGain() {
        AudioTrack activeTrack = audioTrack;
        float activeGain = gain;
        if (activeTrack == null) {
            return;
        }
        try {
            activeTrack.setVolume(Math.min(1.0f, activeGain));
        } catch (RuntimeException error) {
            AppLog.warn("audio volume change failed", error);
        }

        LoudnessEnhancer activeEnhancer = loudnessEnhancer;
        if (activeEnhancer == null) {
            return;
        }
        try {
            activeEnhancer.setTargetGain(gainToMillibels(activeGain));
            activeEnhancer.setEnabled(activeGain > 1.0f);
        } catch (RuntimeException error) {
            AppLog.warn("audio gain boost failed", error);
        }
    }

    static float clampGain(float requestedGain) {
        if (Float.isNaN(requestedGain) || requestedGain < 0f) {
            return 0f;
        }
        return Math.min(MAX_GAIN, requestedGain);
    }

    static int gainToMillibels(float requestedGain) {
        float clamped = clampGain(requestedGain);
        if (clamped <= 1.0f) {
            return 0;
        }
        return (int) Math.round(2000.0 * Math.log10(clamped));
    }

    static int audioFramesForMilliseconds(int milliseconds) {
        return SAMPLE_RATE * milliseconds / 1000;
    }

    static int audioBytesForMilliseconds(int milliseconds) {
        return audioFramesForMilliseconds(milliseconds) * PCM_BYTES_PER_FRAME;
    }

    private static double framesToMilliseconds(int frames) {
        return Math.round(frames * 1000.0 / SAMPLE_RATE * 10.0) / 10.0;
    }

    static int payloadOffset(byte[] packet, int length) {
        if (packet == null || length <= RTP_HEADER_SIZE || length > packet.length) {
            return -1;
        }
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
        return offset <= length ? offset : -1;
    }

    static int readInt(byte[] packet, int offset) {
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

    private static void joinWorker(Thread worker) {
        if (worker == null || worker == Thread.currentThread()) {
            return;
        }
        worker.interrupt();
        try {
            worker.join(1000L);
            if (worker.isAlive()) {
                AppLog.warn("audio receiver thread did not stop within one second", null);
            }
        } catch (InterruptedException error) {
            Thread.currentThread().interrupt();
            AppLog.warn("interrupted while stopping audio receiver", error);
        }
    }
}
