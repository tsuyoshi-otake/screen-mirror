package com.screenmirror;

import android.media.MediaCodec;
import android.media.MediaFormat;
import android.view.Surface;

import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.InetSocketAddress;
import java.net.SocketException;
import java.net.SocketTimeoutException;
import java.nio.ByteBuffer;
import java.util.Arrays;
import java.util.concurrent.atomic.AtomicBoolean;

final class RtpH264Receiver {
    interface Listener {
        void onFirstPacket(String host);

        void onDisconnected();

        void onError(Throwable error);
    }

    private static final byte[] START_CODE = new byte[]{0, 0, 0, 1};
    private static final int SOCKET_BUFFER_SIZE = 1024 * 1024;
    private static final int SOCKET_TIMEOUT_MS = 250;
    private static final int DISCONNECT_TIMEOUT_MS = 3000;
    private static final int MAX_ASSEMBLED_NAL_SIZE = 4 * 1024 * 1024;
    private static final int DSCP_EF_TRAFFIC_CLASS = 0xB8;

    private final AtomicBoolean running = new AtomicBoolean(false);
    private final byte[] receiveBuffer = new byte[2048];
    private final DatagramPacket receivePacket = new DatagramPacket(receiveBuffer, receiveBuffer.length);
    private final FuState fuState = new FuState();
    private final MediaCodec.BufferInfo bufferInfo = new MediaCodec.BufferInfo();
    private final DisconnectWatchdog watchdog = new DisconnectWatchdog(DISCONNECT_TIMEOUT_MS);

    private volatile Listener listener;
    private volatile DatagramSocket socket;
    private Thread thread;
    private MediaCodec decoder;
    private volatile String lastSenderHost;

    void setListener(Listener listener) {
        this.listener = listener;
    }

    synchronized void start(int port, Surface surface) throws Exception {
        stop();

        DatagramSocket localSocket = createSocket(port);
        MediaCodec localDecoder = null;
        try {
            localDecoder = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_VIDEO_AVC);
            MediaFormat format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, 1920, 1080);
            format.setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, 1024 * 1024);
            if (android.os.Build.VERSION.SDK_INT >= 30) {
                format.setInteger(MediaFormat.KEY_LOW_LATENCY, 1);
            }
            localDecoder.configure(format, surface, null, 0);
            localDecoder.start();
        } catch (Exception error) {
            localSocket.close();
            if (localDecoder != null) {
                localDecoder.release();
            }
            throw error;
        }

        decoder = localDecoder;
        socket = localSocket;
        lastSenderHost = null;
        fuState.reset();
        watchdog.reset();
        running.set(true);
        thread = new Thread(() -> receiveLoop(localSocket), "rtp-h264-receiver");
        thread.start();
        AppLog.info("video receiver started on UDP " + port);
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
        joinWorker(worker, "video receiver");

        MediaCodec activeDecoder = decoder;
        decoder = null;
        if (activeDecoder != null) {
            try {
                activeDecoder.stop();
            } catch (Exception error) {
                AppLog.warn("video decoder stop failed", error);
            }
            activeDecoder.release();
        }
        lastSenderHost = null;
        fuState.reset();
        watchdog.reset();
    }

    boolean isRunning() {
        return running.get();
    }

    String lastSenderHost() {
        return lastSenderHost;
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
                AppLog.warn("video receiver could not set DSCP", error);
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
                    String senderHost = receivePacket.getAddress().getHostAddress();
                    lastSenderHost = senderHost;
                    if (watchdog.onPacket()) {
                        AppLog.info("video packets started from " + senderHost);
                        Listener activeListener = listener;
                        if (activeListener != null) {
                            activeListener.onFirstPacket(senderHost);
                        }
                    }
                    if (depacketizeAndQueue(receiveBuffer, receivePacket.getLength())) {
                        drainDecoder();
                    }
                } catch (SocketTimeoutException timeout) {
                    if (watchdog.isTimedOut()) {
                        running.set(false);
                        AppLog.info("video receiver disconnected after packet timeout");
                        Listener activeListener = listener;
                        if (activeListener != null) {
                            activeListener.onDisconnected();
                        }
                    }
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
        AppLog.error("video receiver failed", error);
        Listener activeListener = listener;
        if (activeListener != null) {
            activeListener.onError(error);
        }
    }

    private boolean depacketizeAndQueue(byte[] packet, int length) throws Exception {
        if (length <= 12 || (packet[0] & 0xc0) != 0x80) {
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
        try {
            fuState.append(packet, fragmentOffset, fragmentLength);
        } catch (IllegalArgumentException oversizedNal) {
            fuState.reset();
            AppLog.warn("dropped an oversized fragmented H.264 NAL", oversizedNal);
            return false;
        }
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
        long presentationTimeUs = System.nanoTime() / 1000L;
        if (input == null || input.capacity() < length + START_CODE.length) {
            activeDecoder.queueInputBuffer(index, 0, 0, presentationTimeUs, 0);
            AppLog.warn("video decoder input buffer was too small", null);
            return;
        }

        input.clear();
        input.put(START_CODE);
        input.put(source, offset, length);
        activeDecoder.queueInputBuffer(index, 0, length + START_CODE.length, presentationTimeUs, 0);
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

    private static void joinWorker(Thread worker, String name) {
        if (worker == null || worker == Thread.currentThread()) {
            return;
        }
        worker.interrupt();
        try {
            worker.join(1000L);
            if (worker.isAlive()) {
                AppLog.warn(name + " thread did not stop within one second", null);
            }
        } catch (InterruptedException error) {
            Thread.currentThread().interrupt();
            AppLog.warn("interrupted while stopping " + name, error);
        }
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
            if (length < 0 || size + (long) length > MAX_ASSEMBLED_NAL_SIZE) {
                throw new IllegalArgumentException("fragmented NAL exceeds maximum size");
            }
            if (size + length > data.length) {
                data = Arrays.copyOf(data, Math.max(data.length * 2, size + length));
            }
        }
    }
}
