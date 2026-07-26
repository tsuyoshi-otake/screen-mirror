package com.screenmirror;

import android.media.MediaCodec;
import android.media.MediaFormat;
import android.view.Surface;

import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.InetSocketAddress;
import java.nio.ByteBuffer;
import java.util.Arrays;
import java.util.concurrent.atomic.AtomicBoolean;

final class RtpH264Receiver {
    private static final byte[] START_CODE = new byte[]{0, 0, 0, 1};
    private static final int SOCKET_BUFFER_SIZE = 4 * 1024 * 1024;
    private static final int DSCP_EF_TRAFFIC_CLASS = 0xB8;

    private final AtomicBoolean running = new AtomicBoolean(false);
    private final byte[] receiveBuffer = new byte[2048];
    private final DatagramPacket receivePacket = new DatagramPacket(receiveBuffer, receiveBuffer.length);
    private final FuState fuState = new FuState();
    private final MediaCodec.BufferInfo bufferInfo = new MediaCodec.BufferInfo();

    private Thread thread;
    private MediaCodec decoder;
    private volatile String lastSenderHost;

    void start(int port, Surface surface) throws Exception {
        stop();
        decoder = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_VIDEO_AVC);
        MediaFormat format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, 1920, 1080);
        format.setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, 1024 * 1024);
        if (android.os.Build.VERSION.SDK_INT >= 30) {
            format.setInteger(MediaFormat.KEY_LOW_LATENCY, 1);
        }
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
        fuState.reset();
    }

    String lastSenderHost() {
        return lastSenderHost;
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
                lastSenderHost = receivePacket.getAddress().getHostAddress();
                if (depacketizeAndQueue(receiveBuffer, receivePacket.getLength())) {
                    drainDecoder();
                }
            }
        } catch (Exception ignored) {
        }
    }

    private boolean depacketizeAndQueue(byte[] packet, int length) throws Exception {
        if (length <= 12) {
            return false;
        }

        int csrcCount = packet[0] & 0x0f;
        int payloadOffset = 12 + csrcCount * 4;
        if (payloadOffset >= length) {
            return false;
        }

        int nalType = packet[payloadOffset] & 0x1f;
        if (nalType >= 1 && nalType <= 23) {
            queueNal(packet, payloadOffset, length - payloadOffset);
            return true;
        }

        if (nalType != 28 || payloadOffset + 2 >= length) {
            return false;
        }

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
            return false;
        }
        fuState.append(packet, fragmentOffset, fragmentLength);
        if (end) {
            queueNal(fuState.data, 0, fuState.size);
            fuState.reset();
            return true;
        }
        return false;
    }

    private void queueNal(byte[] source, int offset, int length) throws Exception {
        MediaCodec activeDecoder = decoder;
        if (activeDecoder == null || length <= 0) {
            return;
        }

        int index = activeDecoder.dequeueInputBuffer(1_000);
        if (index < 0) {
            return;
        }

        ByteBuffer input = activeDecoder.getInputBuffer(index);
        if (input == null || input.capacity() < length + START_CODE.length) {
            return;
        }

        input.clear();
        input.put(START_CODE);
        input.put(source, offset, length);
        activeDecoder.queueInputBuffer(index, 0, length + START_CODE.length, System.nanoTime() / 1000, 0);
    }

    private void drainDecoder() {
        MediaCodec activeDecoder = decoder;
        if (activeDecoder == null) {
            return;
        }

        int output;
        do {
            output = activeDecoder.dequeueOutputBuffer(bufferInfo, 0);
            if (output >= 0) {
                activeDecoder.releaseOutputBuffer(output, true);
            }
        } while (output >= 0);
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
