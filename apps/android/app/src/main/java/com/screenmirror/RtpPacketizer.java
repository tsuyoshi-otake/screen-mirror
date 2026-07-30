package com.screenmirror;

import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.InetAddress;
import java.nio.ByteBuffer;
import java.util.List;

final class RtpPacketizer implements AutoCloseable {
    private static final int PAYLOAD_TYPE = 96;
    private static final int CLOCK_RATE = 90000;
    private static final int MTU = 1200;
    private static final int SOCKET_BUFFER_SIZE = 1024 * 1024;
    private static final int DSCP_EF_TRAFFIC_CLASS = 0xB8;
    private static final int RTP_HEADER_SIZE = 12;
    private static final int FU_A_HEADER_SIZE = 2;
    private static final int SINGLE_NAL_MAX_PAYLOAD = MTU - RTP_HEADER_SIZE;
    private static final int FU_A_MAX_PAYLOAD = MTU - RTP_HEADER_SIZE - FU_A_HEADER_SIZE;

    private static final Target[] NO_TARGETS = new Target[0];
    private static final DatagramPacket[] NO_PACKETS = new DatagramPacket[0];
    private static final byte[] NO_FRAME = new byte[0];
    /** Comfortably above a 1080p key frame, so a session settles after one growth at most. */
    private static final int INITIAL_FRAME_BUFFER_SIZE = 512 * 1024;

    private int sequence = 1;
    private final int ssrc = (int) System.nanoTime();
    private final byte[] packetBuffer = new byte[MTU];
    /** The frame is scanned and sliced out of here rather than out of the encoder's direct buffer. */
    private byte[] frameBuffer = NO_FRAME;
    /** Cold: only compared against the peer list. Kept out of {@link #targetPackets} for that reason. */
    private Target[] targets = NO_TARGETS;
    /** Hot: the only array the per-packet send loop touches. */
    private DatagramPacket[] targetPackets = NO_PACKETS;
    private boolean targetsInitialized = false;
    private DatagramSocket socket;

    RtpPacketizer() {
        // The version byte and the SSRC are the same in every packet of a session, so the per-packet
        // header write only has to touch the seven bytes that actually change.
        packetBuffer[0] = (byte) 0x80;
        packetBuffer[8] = (byte) (ssrc >> 24);
        packetBuffer[9] = (byte) (ssrc >> 16);
        packetBuffer[10] = (byte) (ssrc >> 8);
        packetBuffer[11] = (byte) ssrc;
    }

    void sendH264(ByteBuffer encoded, int size, long presentationTimeUs, List<DiscoveryAgent.Peer> peers) throws Exception {
        if (peers.isEmpty() || size <= 0) {
            return;
        }
        if (!targetsInitialized) {
            refreshTargets(peers);
        }
        if (targetPackets.length == 0) {
            return;
        }

        // The encoder hands out a direct buffer, where every get() is a bounds-checked native read
        // and nothing can be hoisted. Both the start-code scan and the payload copies walk most of
        // the frame, so one bulk copy into a reusable array pays for itself twice over and leaves
        // the bytes hot for the packet loop that follows.
        byte[] frame = frameBuffer(size);
        ByteBuffer source = encoded.slice();
        source.limit(size);
        source.get(frame, 0, size);

        DatagramSocket activeSocket = socket();
        int timestamp = (int) ((presentationTimeUs * CLOCK_RATE) / 1_000_000L);
        int startCode = findStartCode(frame, 0, size);

        if (startCode < 0) {
            sendNal(activeSocket, frame, 0, size, timestamp);
            return;
        }

        while (startCode >= 0) {
            int nalStart = startCode + (frame[startCode + 2] == 1 ? 3 : 4);
            int nextStart = findStartCode(frame, nalStart, size);
            int nalEnd = nextStart >= 0 ? nextStart : size;
            if (nalEnd > nalStart) {
                sendNal(activeSocket, frame, nalStart, nalEnd - nalStart, timestamp);
            }
            startCode = nextStart;
        }
    }

    @Override
    public void close() {
        if (socket != null) {
            socket.close();
            socket = null;
        }
        targets = NO_TARGETS;
        targetPackets = NO_PACKETS;
        targetsInitialized = false;
        // A single outsized frame would otherwise keep its buffer for the life of the process.
        frameBuffer = NO_FRAME;
    }

    private byte[] frameBuffer(int size) {
        if (frameBuffer.length < size) {
            frameBuffer = new byte[Math.max(size, Math.max(INITIAL_FRAME_BUFFER_SIZE, frameBuffer.length * 2))];
        }
        return frameBuffer;
    }

    private void sendNal(DatagramSocket activeSocket, byte[] frame, int nalOffset, int nalLength, int timestamp) throws Exception {
        if (nalLength <= SINGLE_NAL_MAX_PAYLOAD) {
            writeRtpHeader(timestamp, true);
            System.arraycopy(frame, nalOffset, packetBuffer, RTP_HEADER_SIZE, nalLength);
            sendPacketToTargets(activeSocket, RTP_HEADER_SIZE + nalLength);
            return;
        }

        int nalHeader = frame[nalOffset] & 0xff;
        int fuIndicator = (nalHeader & 0xe0) | 28;
        int nalType = nalHeader & 0x1f;
        int offset = nalOffset + 1;
        int remaining = nalLength - 1;
        boolean start = true;

        while (remaining > 0) {
            int payload = Math.min(FU_A_MAX_PAYLOAD, remaining);
            boolean end = payload == remaining;
            writeRtpHeader(timestamp, end);
            packetBuffer[RTP_HEADER_SIZE] = (byte) fuIndicator;
            packetBuffer[RTP_HEADER_SIZE + 1] = (byte) ((start ? 0x80 : 0) | (end ? 0x40 : 0) | nalType);
            System.arraycopy(frame, offset, packetBuffer, RTP_HEADER_SIZE + FU_A_HEADER_SIZE, payload);
            sendPacketToTargets(activeSocket, RTP_HEADER_SIZE + FU_A_HEADER_SIZE + payload);
            offset += payload;
            remaining -= payload;
            start = false;
        }
    }

    private void sendPacketToTargets(DatagramSocket activeSocket, int packetLength) throws Exception {
        DatagramPacket[] packets = targetPackets;
        for (int i = 0; i < packets.length; i++) {
            DatagramPacket packet = packets[i];
            packet.setLength(packetLength);
            activeSocket.send(packet);
        }
    }

    private DatagramSocket socket() throws Exception {
        if (socket == null || socket.isClosed()) {
            socket = new DatagramSocket();
            socket.setSendBufferSize(SOCKET_BUFFER_SIZE);
            try {
                socket.setTrafficClass(DSCP_EF_TRAFFIC_CLASS);
            } catch (Exception error) {
                AppLog.warn("video sender could not set DSCP", error);
            }
        }
        return socket;
    }

    /** Writes only the seven bytes that change per packet; the rest was written in the constructor. */
    private void writeRtpHeader(int timestamp, boolean marker) {
        int seq = sequence++ & 0xffff;
        packetBuffer[1] = (byte) (marker ? PAYLOAD_TYPE | 0x80 : PAYLOAD_TYPE);
        packetBuffer[2] = (byte) (seq >> 8);
        packetBuffer[3] = (byte) seq;
        packetBuffer[4] = (byte) (timestamp >> 24);
        packetBuffer[5] = (byte) (timestamp >> 16);
        packetBuffer[6] = (byte) (timestamp >> 8);
        packetBuffer[7] = (byte) timestamp;
    }

    private void refreshTargets(List<DiscoveryAgent.Peer> peers) throws Exception {
        if (targetsMatch(peers)) {
            targetsInitialized = true;
            return;
        }

        Target[] refreshed = new Target[peers.size()];
        DatagramPacket[] packets = new DatagramPacket[peers.size()];
        for (int i = 0; i < peers.size(); i++) {
            DiscoveryAgent.Peer peer = peers.get(i);
            InetAddress address = InetAddress.getByName(peer.host);
            packets[i] = new DatagramPacket(packetBuffer, MTU, address, peer.streamPort);
            refreshed[i] = new Target(peer.host, peer.streamPort);
        }
        targets = refreshed;
        targetPackets = packets;
        targetsInitialized = true;
    }

    private boolean targetsMatch(List<DiscoveryAgent.Peer> peers) {
        if (peers.size() != targets.length) {
            return false;
        }
        for (int i = 0; i < peers.size(); i++) {
            DiscoveryAgent.Peer peer = peers.get(i);
            Target target = targets[i];
            if (peer.streamPort != target.port || !peer.host.equals(target.host)) {
                return false;
            }
        }
        return true;
    }

    /**
     * Index of the next Annex-B start code, three-byte or four-byte, at or after {@code offset}.
     *
     * <p>Encoded video is overwhelmingly non-zero, so the scan is driven off the third byte of the
     * window: unless it is zero, no start code can begin at any of the three positions it covers and
     * the whole window is skipped. Only a zero there forces a single-byte step, which is what the
     * four-byte form needs to be found at its true first byte rather than one byte in.
     */
    // Visible for testing: the skipping scan has to agree with the naive one on every boundary.
    static int findStartCode(byte[] data, int offset, int limit) {
        int i = offset;
        while (i + 2 < limit) {
            byte third = data[i + 2];
            if (third != 0) {
                if (third == 1 && data[i] == 0 && data[i + 1] == 0) {
                    return i;
                }
                i += 3;
                continue;
            }
            if (data[i] == 0 && data[i + 1] == 0 && i + 3 < limit && data[i + 3] == 1) {
                return i;
            }
            i++;
        }
        return -1;
    }

    private static final class Target {
        final String host;
        final int port;

        Target(String host, int port) {
            this.host = host;
            this.port = port;
        }
    }
}
