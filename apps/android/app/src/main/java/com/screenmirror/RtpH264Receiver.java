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

        /** Reports visible crop and coded buffer dimensions so the view can letterbox correctly. */
        void onVideoSize(int width, int height, int codedWidth, int codedHeight);

        void onDisconnected();

        void onError(Throwable error);
    }

    private static final byte[] START_CODE = new byte[]{0, 0, 0, 1};
    private static final int SOCKET_BUFFER_SIZE = 1024 * 1024;
    private static final int SOCKET_TIMEOUT_MS = 250;
    private static final int DISCONNECT_TIMEOUT_MS = 3000;
    private static final int MAX_ASSEMBLED_NAL_SIZE = 4 * 1024 * 1024;
    private static final int DSCP_EF_TRAFFIC_CLASS = 0xB8;
    private static final int NAL_TYPE_IDR = 5;
    private static final int NAL_TYPE_SPS = 7;
    private static final int NAL_TYPE_PPS = 8;
    private static final int NAL_TYPE_STAP_A = 24;
    private static final int NAL_TYPE_FU_A = 28;
    private static final int RTP_HEADER_SIZE = 12;
    private static final int H264_PAYLOAD_TYPE = 96;

    private final AtomicBoolean running = new AtomicBoolean(false);
    private final byte[] receiveBuffer = new byte[2048];
    private final DatagramPacket receivePacket = new DatagramPacket(receiveBuffer, receiveBuffer.length);
    private final FuState fuState = new FuState();
    private final MediaCodec.BufferInfo bufferInfo = new MediaCodec.BufferInfo();
    private final DisconnectWatchdog watchdog;
    private final RtpStreamLock streamLock = new RtpStreamLock();

    private volatile Listener listener;
    private volatile DatagramSocket socket;
    private Thread thread;
    /**
     * The decoder outlives no lock-free access: the receive thread feeds it while the UI thread may
     * tear the session down, so every touch happens under {@link #decoderLock}.
     */
    private final Object decoderLock = new Object();
    private MediaCodec decoder;
    private Surface decoderSurface;
    private boolean awaitingKeyFrame = true;
    private boolean renderedAFrame;
    private boolean warnedAboutMalformedAggregation;
    private int reportedVideoWidth;
    private int reportedVideoHeight;
    private int reportedBufferWidth;
    private int reportedBufferHeight;
    // No reordering buffer is intentional: late fragments are discarded rather than increasing
    // latency or contaminating a decoded access unit.
    private int expectedSequence = -1;
    private int lastRtpTimestamp;
    private boolean hasLastRtpTimestamp;
    private boolean droppingAccessUnit;
    private int droppedAccessUnitTimestamp;
    private volatile String lastSenderHost;

    RtpH264Receiver() {
        this(new DisconnectWatchdog(DISCONNECT_TIMEOUT_MS));
    }

    // Package-private for deterministic JVM tests of stream takeover timing.
    RtpH264Receiver(DisconnectWatchdog watchdog) {
        if (watchdog == null) {
            throw new IllegalArgumentException("watchdog is required");
        }
        this.watchdog = watchdog;
    }

    void setListener(Listener listener) {
        this.listener = listener;
    }

    synchronized void start(int port, Surface surface) throws Exception {
        stop();

        DatagramSocket localSocket = createSocket(port);
        MediaCodec localDecoder;
        try {
            localDecoder = createDecoder(surface);
        } catch (Exception error) {
            localSocket.close();
            throw error;
        }

        synchronized (decoderLock) {
            decoder = localDecoder;
            decoderSurface = surface;
            awaitingKeyFrame = true;
            renderedAFrame = false;
            warnedAboutMalformedAggregation = false;
            clearReportedVideoSizeLocked();
        }
        socket = localSocket;
        lastSenderHost = null;
        resetPacketTracking();
        watchdog.reset();
        streamLock.reset();
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

        releaseDecoder();
        lastSenderHost = null;
        resetPacketTracking();
        watchdog.reset();
        streamLock.reset();
    }

    boolean isRunning() {
        return running.get();
    }

    String lastSenderHost() {
        return lastSenderHost;
    }

    private static MediaCodec createDecoder(Surface surface) throws Exception {
        MediaCodec localDecoder = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_VIDEO_AVC);
        try {
            MediaFormat format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, 1920, 1080);
            format.setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, 1024 * 1024);
            if (android.os.Build.VERSION.SDK_INT >= 30) {
                format.setInteger(MediaFormat.KEY_LOW_LATENCY, 1);
            }
            localDecoder.configure(format, surface, null, 0);
            localDecoder.start();
            return localDecoder;
        } catch (Exception error) {
            localDecoder.release();
            throw error;
        }
    }

    private void releaseDecoder() {
        MediaCodec activeDecoder;
        synchronized (decoderLock) {
            activeDecoder = decoder;
            decoder = null;
            decoderSurface = null;
            awaitingKeyFrame = true;
            clearReportedVideoSizeLocked();
        }
        if (activeDecoder == null) {
            return;
        }
        try {
            activeDecoder.stop();
        } catch (Exception error) {
            AppLog.warn("video decoder stop failed", error);
        }
        activeDecoder.release();
    }

    /**
     * A codec that dies mid-stream used to take the whole session with it. Rebuild it on the same
     * surface instead and resync on the next key frame; the sender emits one every second.
     */
    private void restartDecoderLocked(Throwable error) {
        MediaCodec failed = decoder;
        Surface surface = decoderSurface;
        decoder = null;
        awaitingKeyFrame = true;
        if (failed != null) {
            try {
                failed.stop();
            } catch (Exception ignored) {
                // The codec is already unusable; releasing it is all that still matters.
            }
            failed.release();
        }
        if (!running.get() || surface == null || !surface.isValid()) {
            decoderSurface = null;
            AppLog.warn("video decoder stopped and its surface is gone", error);
            return;
        }
        try {
            decoder = createDecoder(surface);
            AppLog.warn("video decoder restarted after an error", error);
        } catch (Exception failure) {
            decoderSurface = null;
            fail(failure);
        }
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
                    RtpHeader header = selectRtpHeader(senderHost, receiveBuffer, receivePacket.getLength());
                    if (header == null) {
                        disconnectIfSelectedStreamTimedOut();
                        continue;
                    }
                    lastSenderHost = senderHost;
                    if (watchdog.onPacket()) {
                        AppLog.info("video packets started from " + senderHost);
                        Listener activeListener = listener;
                        if (activeListener != null) {
                            activeListener.onFirstPacket(senderHost);
                        }
                    }
                    depacketizeAndQueue(receiveBuffer, header);
                    // Draining unconditionally: skipping it while the input queue is full starves
                    // the codec of buffers and it eventually gives up with a fatal error.
                    drainDecoder();
                } catch (SocketTimeoutException timeout) {
                    disconnectIfSelectedStreamTimedOut();
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

    /** Invalid traffic must not suppress the timeout of a stream that had already been selected. */
    private boolean disconnectIfSelectedStreamTimedOut() {
        if (!watchdog.isTimedOut()) {
            return false;
        }
        running.set(false);
        AppLog.info("video receiver disconnected after selected stream timeout");
        Listener activeListener = listener;
        if (activeListener != null) {
            activeListener.onDisconnected();
        }
        return true;
    }

    // Visible for testing: depacketization is pure parsing and needs no decoder to be exercised.
    boolean depacketizeAndQueue(byte[] packet, int length) throws Exception {
        RtpHeader header = RtpHeader.parse(packet, length);
        if (header == null) {
            return false;
        }
        return depacketizeAndQueue(packet, header);
    }

    /** Visible for testing: admits exactly one valid RTP sender stream per receiver session. */
    boolean acceptsStreamPacket(String senderHost, byte[] packet, int length) {
        RtpHeader header = selectRtpHeader(senderHost, packet, length);
        if (header == null) {
            return false;
        }
        watchdog.onPacket();
        return true;
    }

    /**
     * Reject packets from every other stream, but do not let that traffic mask a dead selected
     * stream forever. Once the selected stream has been silent for the normal watchdog interval,
     * make the current valid candidate the new stream and wait for its SPS/IDR again.
     */
    private RtpHeader selectRtpHeader(String senderHost, byte[] packet, int length) {
        RtpHeader header = RtpHeader.parse(packet, length);
        if (header == null) {
            return null;
        }
        if (streamLock.accepts(senderHost, header.ssrc)) {
            return header;
        }
        if (watchdog.isTimedOut()) {
            AppLog.warn("selected RTP video stream timed out; switching from "
                    + streamLock.description() + " to " + senderHost + "/"
                    + Integer.toUnsignedString(header.ssrc), null);
            resyncForStreamTakeover();
            streamLock.reset();
            streamLock.accepts(senderHost, header.ssrc);
            watchdog.reset();
            return header;
        }
        if (streamLock.shouldWarnFor(senderHost, header.ssrc)) {
            AppLog.warn("ignoring RTP video stream from " + senderHost
                    + " with SSRC " + Integer.toUnsignedString(header.ssrc)
                    + "; receiver is locked to " + streamLock.description(), null);
        }
        return null;
    }

    private void resyncForStreamTakeover() {
        resetPacketTracking();
        synchronized (decoderLock) {
            awaitingKeyFrame = true;
            renderedAFrame = false;
        }
    }

    private void resetPacketTracking() {
        fuState.reset();
        expectedSequence = -1;
        hasLastRtpTimestamp = false;
        droppingAccessUnit = false;
    }

    /** Marks a sequence gap as damage and prevents the rest of that access unit from rendering. */
    private void notePacketSequence(RtpHeader header) {
        if (expectedSequence >= 0 && header.sequence != expectedSequence) {
            boolean sameAccessUnit = hasLastRtpTimestamp && header.timestamp == lastRtpTimestamp;
            AppLog.warn("RTP sequence gap: expected " + expectedSequence + ", got " + header.sequence,
                    null);
            fuState.reset();
            synchronized (decoderLock) {
                awaitingKeyFrame = true;
            }
            if (sameAccessUnit) {
                droppingAccessUnit = true;
                droppedAccessUnitTimestamp = header.timestamp;
            }
        }
        expectedSequence = (header.sequence + 1) & 0xffff;
        lastRtpTimestamp = header.timestamp;
        hasLastRtpTimestamp = true;
    }

    private boolean shouldDropAccessUnit(int timestamp) {
        if (!droppingAccessUnit) {
            return false;
        }
        if (timestamp == droppedAccessUnitTimestamp) {
            return true;
        }
        droppingAccessUnit = false;
        return false;
    }

    private void discardIncompleteNal(int timestamp) {
        fuState.reset();
        discardAccessUnit(timestamp);
    }

    private void discardAccessUnit(int timestamp) {
        droppingAccessUnit = true;
        droppedAccessUnitTimestamp = timestamp;
        synchronized (decoderLock) {
            awaitingKeyFrame = true;
        }
    }

    private boolean depacketizeAndQueue(byte[] packet, RtpHeader header) throws Exception {
        notePacketSequence(header);
        if (shouldDropAccessUnit(header.timestamp)) {
            return false;
        }
        int payloadOffset = header.payloadOffset;
        int payloadLength = header.payloadLength;

        int nalType = packet[payloadOffset] & 0x1f;
        if (nalType >= 1 && nalType <= 23) {
            queueNal(packet, payloadOffset, payloadLength, header.timestamp);
            return true;
        }

        // The sender aggregates its parameter sets, so a stream without STAP-A support never decodes.
        if (nalType == NAL_TYPE_STAP_A) {
            return queueAggregatedNals(packet, payloadOffset + 1, payloadLength - 1, header.timestamp);
        }

        if (nalType != NAL_TYPE_FU_A || payloadLength < 3) {
            return false;
        }

        int fuIndicator = packet[payloadOffset] & 0xff;
        int fuHeader = packet[payloadOffset + 1] & 0xff;
        boolean start = (fuHeader & 0x80) != 0;
        boolean end = (fuHeader & 0x40) != 0;
        int reconstructedHeader = (fuIndicator & 0xe0) | (fuHeader & 0x1f);
        int fragmentOffset = payloadOffset + 2;
        int fragmentLength = payloadLength - 2;

        if (start) {
            if (fuState.active) {
                // The unfinished NAL is corrupt. Without an access-unit buffer, conservatively
                // drop this whole timestamp instead of combining an unknown new slice with it.
                discardIncompleteNal(header.timestamp);
                return false;
            }
            fuState.reset();
            fuState.start((byte) reconstructedHeader, header.timestamp);
        }
        if (!fuState.active || fuState.timestamp != header.timestamp
                || fuState.nalHeader != (byte) reconstructedHeader) {
            discardIncompleteNal(header.timestamp);
            return false;
        }
        try {
            fuState.append(packet, fragmentOffset, fragmentLength);
        } catch (IllegalArgumentException oversizedNal) {
            discardIncompleteNal(header.timestamp);
            AppLog.warn("dropped an oversized fragmented H.264 NAL", oversizedNal);
            return false;
        }
        if (end) {
            queueNal(fuState.data, 0, fuState.size, header.timestamp);
            fuState.reset();
            return true;
        }
        return false;
    }

    /** Unpacks a STAP-A packet: a series of 2-byte lengths, each followed by one whole NAL unit. */
    private boolean queueAggregatedNals(byte[] packet, int offset, int available, int timestamp) {
        boolean queued = false;
        int cursor = offset;
        int end = offset + available;
        while (cursor + 2 <= end) {
            int size = ((packet[cursor] & 0xff) << 8) | (packet[cursor + 1] & 0xff);
            cursor += 2;
            if (size <= 0 || cursor + size > end) {
                if (!warnedAboutMalformedAggregation) {
                    warnedAboutMalformedAggregation = true;
                    AppLog.warn("dropped a malformed STAP-A packet", null);
                }
                break;
            }
            queueNal(packet, cursor, size, timestamp);
            cursor += size;
            queued = true;
        }
        return queued;
    }

    private void queueNal(byte[] source, int offset, int length, int timestamp) {
        if (length <= 0) {
            return;
        }
        int nalType = source[offset] & 0x1f;

        synchronized (decoderLock) {
            // A fresh codec chokes on data that starts mid-picture. Parameter sets are needed
            // before an IDR, but are not themselves a recovery point after a packet loss.
            if (awaitingKeyFrame) {
                if (nalType != NAL_TYPE_SPS && nalType != NAL_TYPE_PPS && nalType != NAL_TYPE_IDR) {
                    return;
                }
                if (nalType == NAL_TYPE_IDR) {
                    awaitingKeyFrame = false;
                }
            }
            MediaCodec activeDecoder = decoder;
            if (activeDecoder == null) {
                return;
            }

            try {
                int index = activeDecoder.dequeueInputBuffer(10_000);
                if (index < 0) {
                    // Half a picture is worse than none: resync on the next key frame instead.
                    discardAccessUnit(timestamp);
                    return;
                }

                ByteBuffer input = activeDecoder.getInputBuffer(index);
                long presentationTimeUs = System.nanoTime() / 1000L;
                if (input == null || input.capacity() < length + START_CODE.length) {
                    activeDecoder.queueInputBuffer(index, 0, 0, presentationTimeUs, 0);
                    discardAccessUnit(timestamp);
                    AppLog.warn("video decoder input buffer was too small", null);
                    return;
                }

                input.clear();
                input.put(START_CODE);
                input.put(source, offset, length);
                activeDecoder.queueInputBuffer(index, 0, length + START_CODE.length, presentationTimeUs, 0);
            } catch (IllegalStateException error) {
                restartDecoderLocked(error);
                discardAccessUnit(timestamp);
            }
        }
    }

    // Visible for testing: validates recovery state without requiring an Android MediaCodec.
    boolean isAwaitingKeyFrame() {
        synchronized (decoderLock) {
            return awaitingKeyFrame;
        }
    }

    /**
     * The decoder reports the real picture size only once it has parsed the parameter sets, and the
     * crop rectangle is what actually reaches the surface.
     */
    private void reportVideoSize(MediaFormat format) {
        int codedWidth = format.containsKey(MediaFormat.KEY_WIDTH)
                ? format.getInteger(MediaFormat.KEY_WIDTH) : 0;
        int codedHeight = format.containsKey(MediaFormat.KEY_HEIGHT)
                ? format.getInteger(MediaFormat.KEY_HEIGHT) : 0;
        int width = videoSize(format, codedWidth, "crop-left", "crop-right");
        int height = videoSize(format, codedHeight, "crop-top", "crop-bottom");
        if (width <= 0 || height <= 0) {
            return;
        }
        if (codedWidth <= 0) {
            codedWidth = width;
        }
        if (codedHeight <= 0) {
            codedHeight = height;
        }
        if (width == reportedVideoWidth && height == reportedVideoHeight
                && codedWidth == reportedBufferWidth && codedHeight == reportedBufferHeight) {
            return;
        }
        reportedVideoWidth = width;
        reportedVideoHeight = height;
        reportedBufferWidth = codedWidth;
        reportedBufferHeight = codedHeight;
        AppLog.info("video size is " + width + "x" + height
                + " (coded " + codedWidth + "x" + codedHeight + ")");
        Listener activeListener = listener;
        if (activeListener != null) {
            activeListener.onVideoSize(width, height, codedWidth, codedHeight);
        }
    }

    private static int videoSize(MediaFormat format, int defaultSize, String startKey, String endKey) {
        if (format.containsKey(startKey) && format.containsKey(endKey)) {
            return format.getInteger(endKey) - format.getInteger(startKey) + 1;
        }
        return defaultSize;
    }

    private void clearReportedVideoSizeLocked() {
        reportedVideoWidth = 0;
        reportedVideoHeight = 0;
        reportedBufferWidth = 0;
        reportedBufferHeight = 0;
    }

    private void drainDecoder() {
        synchronized (decoderLock) {
            MediaCodec activeDecoder = decoder;
            if (activeDecoder == null) {
                return;
            }

            try {
                int output;
                do {
                    output = activeDecoder.dequeueOutputBuffer(bufferInfo, 0);
                    if (output == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED) {
                        reportVideoSize(activeDecoder.getOutputFormat());
                    } else if (output >= 0) {
                        activeDecoder.releaseOutputBuffer(output, true);
                        if (!renderedAFrame) {
                            renderedAFrame = true;
                            AppLog.info("video decoder rendered its first frame");
                        }
                    }
                } while (output >= 0 || output == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED);
            } catch (IllegalStateException error) {
                restartDecoderLocked(error);
            }
        }
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

    /** A session chooses the first valid H.264 RTP stream and never interleaves another one. */
    private static final class RtpStreamLock {
        private String host;
        private int ssrc;
        private String lastRejected;

        synchronized boolean accepts(String candidateHost, int candidateSsrc) {
            if (host == null) {
                host = candidateHost;
                ssrc = candidateSsrc;
                lastRejected = null;
                return true;
            }
            return host.equals(candidateHost) && ssrc == candidateSsrc;
        }

        synchronized boolean shouldWarnFor(String candidateHost, int candidateSsrc) {
            String candidate = candidateHost + "/" + Integer.toUnsignedString(candidateSsrc);
            if (candidate.equals(lastRejected)) {
                return false;
            }
            lastRejected = candidate;
            return true;
        }

        synchronized String description() {
            return host + "/" + Integer.toUnsignedString(ssrc);
        }

        synchronized void reset() {
            host = null;
            ssrc = 0;
            lastRejected = null;
        }
    }

    /** Validates the RTP envelope before any packet can select or feed the decoder. */
    private static final class RtpHeader {
        final int ssrc;
        final int sequence;
        final int timestamp;
        final int payloadOffset;
        final int payloadLength;

        RtpHeader(int ssrc, int sequence, int timestamp, int payloadOffset, int payloadLength) {
            this.ssrc = ssrc;
            this.sequence = sequence;
            this.timestamp = timestamp;
            this.payloadOffset = payloadOffset;
            this.payloadLength = payloadLength;
        }

        static RtpHeader parse(byte[] packet, int length) {
            if (packet == null || length <= RTP_HEADER_SIZE || length > packet.length) {
                return null;
            }
            int first = packet[0] & 0xff;
            if ((first & 0xc0) != 0x80 || (packet[1] & 0x7f) != H264_PAYLOAD_TYPE) {
                return null;
            }

            int payloadEnd = length;
            if ((first & 0x20) != 0) {
                int padding = packet[length - 1] & 0xff;
                if (padding == 0 || padding > length - RTP_HEADER_SIZE) {
                    return null;
                }
                payloadEnd -= padding;
            }

            int payloadOffset = RTP_HEADER_SIZE + (first & 0x0f) * 4;
            if (payloadOffset > payloadEnd) {
                return null;
            }
            if ((first & 0x10) != 0) {
                if (payloadOffset + 4 > payloadEnd) {
                    return null;
                }
                int extensionWords = ((packet[payloadOffset + 2] & 0xff) << 8)
                        | (packet[payloadOffset + 3] & 0xff);
                long extensionEnd = (long) payloadOffset + 4L + (long) extensionWords * 4L;
                if (extensionEnd > payloadEnd) {
                    return null;
                }
                payloadOffset = (int) extensionEnd;
            }
            if (payloadOffset >= payloadEnd) {
                return null;
            }

            int ssrc = ((packet[8] & 0xff) << 24)
                    | ((packet[9] & 0xff) << 16)
                    | ((packet[10] & 0xff) << 8)
                    | (packet[11] & 0xff);
            int sequence = ((packet[2] & 0xff) << 8) | (packet[3] & 0xff);
            int timestamp = ((packet[4] & 0xff) << 24)
                    | ((packet[5] & 0xff) << 16)
                    | ((packet[6] & 0xff) << 8)
                    | (packet[7] & 0xff);
            return new RtpHeader(ssrc, sequence, timestamp, payloadOffset, payloadEnd - payloadOffset);
        }
    }

    private static final class FuState {
        byte[] data = new byte[1024 * 1024];
        int size = 0;
        boolean active = false;
        byte nalHeader;
        int timestamp;

        void start(byte header, int timestamp) {
            reset();
            nalHeader = header;
            this.timestamp = timestamp;
            append(header);
        }

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
            nalHeader = 0;
            timestamp = 0;
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
