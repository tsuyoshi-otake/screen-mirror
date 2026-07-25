package com.screenmirror;

import android.media.MediaCodec;
import android.media.MediaFormat;
import android.view.Surface;

import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.nio.ByteBuffer;
import java.util.Arrays;
import java.util.concurrent.atomic.AtomicBoolean;

final class RtpH264Receiver {
    private final AtomicBoolean running = new AtomicBoolean(false);
    private Thread thread;
    private MediaCodec decoder;
    private volatile String lastSenderHost;

    void start(int port, Surface surface) throws Exception {
        stop();
        decoder = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_VIDEO_AVC);
        MediaFormat format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, 1920, 1080);
        format.setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, 1024 * 1024);
        decoder.configure(format, surface, null, 0);
        decoder.start();
        running.set(true);
        thread = new Thread(() -> receiveLoop(port), "rtp-h264-receiver");
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
    }

    private void receiveLoop(int port) {
        byte[] buffer = new byte[2048];
        FuState fuState = new FuState();
        try (DatagramSocket socket = new DatagramSocket(port)) {
            socket.setReceiveBufferSize(2 * 1024 * 1024);
            while (running.get()) {
                DatagramPacket packet = new DatagramPacket(buffer, buffer.length);
                socket.receive(packet);
                lastSenderHost = packet.getAddress().getHostAddress();
                byte[] nal = depacketize(packet.getData(), packet.getLength(), fuState);
                if (nal != null) {
                    queueNal(nal);
                }
                drainDecoder();
            }
        } catch (Exception ignored) {
        }
    }

    String lastSenderHost() {
        return lastSenderHost;
    }

    private byte[] depacketize(byte[] packet, int length, FuState fuState) {
        if (length <= 12) {
            return null;
        }
        int csrcCount = packet[0] & 0x0f;
        int payloadOffset = 12 + csrcCount * 4;
        if (payloadOffset >= length) {
            return null;
        }
        int nalType = packet[payloadOffset] & 0x1f;
        if (nalType >= 1 && nalType <= 23) {
            return withStartCode(packet, payloadOffset, length - payloadOffset);
        }
        if (nalType == 28 && payloadOffset + 2 < length) {
            int fuIndicator = packet[payloadOffset] & 0xff;
            int fuHeader = packet[payloadOffset + 1] & 0xff;
            boolean start = (fuHeader & 0x80) != 0;
            boolean end = (fuHeader & 0x40) != 0;
            int reconstructedHeader = (fuIndicator & 0xe0) | (fuHeader & 0x1f);
            int fragmentOffset = payloadOffset + 2;
            int fragmentLength = length - fragmentOffset;
            if (start) {
                fuState.reset();
                fuState.append((byte) reconstructedHeader);
            }
            if (!fuState.active) {
                return null;
            }
            fuState.append(packet, fragmentOffset, fragmentLength);
            if (end) {
                byte[] nal = withStartCode(fuState.data, 0, fuState.size);
                fuState.reset();
                return nal;
            }
        }
        return null;
    }

    private void queueNal(byte[] nal) throws Exception {
        if (decoder == null) {
            return;
        }
        int index = decoder.dequeueInputBuffer(5_000);
        if (index < 0) {
            return;
        }
        ByteBuffer input = decoder.getInputBuffer(index);
        if (input == null) {
            return;
        }
        input.clear();
        input.put(nal);
        decoder.queueInputBuffer(index, 0, nal.length, System.nanoTime() / 1000, 0);
    }

    private void drainDecoder() {
        if (decoder == null) {
            return;
        }
        MediaCodec.BufferInfo info = new MediaCodec.BufferInfo();
        int output;
        do {
            output = decoder.dequeueOutputBuffer(info, 0);
            if (output >= 0) {
                decoder.releaseOutputBuffer(output, true);
            }
        } while (output >= 0);
    }

    private static byte[] withStartCode(byte[] source, int offset, int length) {
        byte[] out = new byte[length + 4];
        out[0] = 0;
        out[1] = 0;
        out[2] = 0;
        out[3] = 1;
        System.arraycopy(source, offset, out, 4, length);
        return out;
    }

    private static final class FuState {
        byte[] data = new byte[1024 * 1024];
        int size = 0;
        boolean active = false;

        void append(byte value) {
            ensure(1);
            data[size++] = value;
            active = true;
        }

        void append(byte[] source, int offset, int length) {
            ensure(length);
            System.arraycopy(source, offset, data, size, length);
            size += length;
            active = true;
        }

        void reset() {
            size = 0;
            active = false;
        }

        void ensure(int length) {
            if (size + length > data.length) {
                data = Arrays.copyOf(data, Math.max(data.length * 2, size + length));
            }
        }
    }
}
