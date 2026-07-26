package com.screenmirror;

import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.InetAddress;
import java.nio.ByteBuffer;
import java.util.ArrayList;
import java.util.List;

final class RtpPacketizer implements AutoCloseable {
    private static final int PAYLOAD_TYPE = 96;
    private static final int CLOCK_RATE = 90000;
    private static final int MTU = 1200;
    private static final int SOCKET_BUFFER_SIZE = 4 * 1024 * 1024;
    private static final int DSCP_EF_TRAFFIC_CLASS = 0xB8;
    private static final int RTP_HEADER_SIZE = 12;
    private static final int FU_A_HEADER_SIZE = 2;
    private static final int SINGLE_NAL_MAX_PAYLOAD = MTU - RTP_HEADER_SIZE;
    private static final int FU_A_MAX_PAYLOAD = MTU - RTP_HEADER_SIZE - FU_A_HEADER_SIZE;

    private int sequence = 1;
    private final int ssrc = (int) System.nanoTime();
    private final byte[] packetBuffer = new byte[MTU];
    private final ArrayList<Target> targets = new ArrayList<>(3);
    private boolean targetsInitialized = false;
    private DatagramSocket socket;

    void sendH264(ByteBuffer encoded, int size, long presentationTimeUs, List<DiscoveryAgent.Peer> peers) throws Exception {
        if (peers.isEmpty() || size <= 0) {
            return;
        }
        if (!targetsInitialized) {
            refreshTargets(peers);
        }
        if (targets.isEmpty()) {
            return;
        }

        ByteBuffer frame = encoded.slice();
        frame.limit(size);
        ByteBuffer copyView = frame.duplicate();
        int timestamp = (int) ((presentationTimeUs * CLOCK_RATE) / 1_000_000L);
        int startCode = findStartCode(frame, 0, size);

        if (startCode < 0) {
            sendNal(frame, copyView, 0, size, timestamp);
            return;
        }

        while (startCode >= 0) {
            int nalStart = startCode + (frame.get(startCode + 2) == 1 ? 3 : 4);
            int nextStart = findStartCode(frame, nalStart, size);
            int nalEnd = nextStart >= 0 ? nextStart : size;
            if (nalEnd > nalStart) {
                sendNal(frame, copyView, nalStart, nalEnd - nalStart, timestamp);
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
        targets.clear();
        targetsInitialized = false;
    }

    private void sendNal(ByteBuffer frame, ByteBuffer copyView, int nalOffset, int nalLength, int timestamp) throws Exception {
        if (nalLength <= SINGLE_NAL_MAX_PAYLOAD) {
            writeRtpHeader(timestamp, true);
            copyFromBuffer(frame, copyView, nalOffset, packetBuffer, RTP_HEADER_SIZE, nalLength);
            sendPacketToTargets(RTP_HEADER_SIZE + nalLength);
            return;
        }

        int nalHeader = frame.get(nalOffset) & 0xff;
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
            copyFromBuffer(frame, copyView, offset, packetBuffer, RTP_HEADER_SIZE + FU_A_HEADER_SIZE, payload);
            sendPacketToTargets(RTP_HEADER_SIZE + FU_A_HEADER_SIZE + payload);
            offset += payload;
            remaining -= payload;
            start = false;
        }
    }

    private void sendPacketToTargets(int packetLength) throws Exception {
        DatagramSocket activeSocket = socket();
        if (targets.size() == 1) {
            DatagramPacket packet = targets.get(0).packet;
            packet.setLength(packetLength);
            activeSocket.send(packet);
            return;
        }
        for (int i = 0; i < targets.size(); i++) {
            DatagramPacket packet = targets.get(i).packet;
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
            } catch (Exception ignored) {
            }
        }
        return socket;
    }

    private void writeRtpHeader(int timestamp, boolean marker) {
        int seq = sequence++ & 0xffff;
        packetBuffer[0] = (byte) 0x80;
        packetBuffer[1] = (byte) (PAYLOAD_TYPE | (marker ? 0x80 : 0));
        packetBuffer[2] = (byte) (seq >> 8);
        packetBuffer[3] = (byte) seq;
        packetBuffer[4] = (byte) (timestamp >> 24);
        packetBuffer[5] = (byte) (timestamp >> 16);
        packetBuffer[6] = (byte) (timestamp >> 8);
        packetBuffer[7] = (byte) timestamp;
        packetBuffer[8] = (byte) (ssrc >> 24);
        packetBuffer[9] = (byte) (ssrc >> 16);
        packetBuffer[10] = (byte) (ssrc >> 8);
        packetBuffer[11] = (byte) ssrc;
    }

    private void refreshTargets(List<DiscoveryAgent.Peer> peers) throws Exception {
        if (targetsMatch(peers)) {
            targetsInitialized = true;
            return;
        }

        targets.clear();
        for (int i = 0; i < peers.size(); i++) {
            DiscoveryAgent.Peer peer = peers.get(i);
            InetAddress address = InetAddress.getByName(peer.host);
            targets.add(new Target(
                peer.host,
                peer.streamPort,
                new DatagramPacket(packetBuffer, MTU, address, peer.streamPort)
            ));
        }
        targetsInitialized = true;
    }

    private static void copyFromBuffer(ByteBuffer source, ByteBuffer copyView, int sourceOffset, byte[] target, int targetOffset, int length) {
        if (source.hasArray()) {
            System.arraycopy(
                    source.array(),
                    source.arrayOffset() + sourceOffset,
                    target,
                    targetOffset,
                    length
            );
            return;
        }
        copyView.clear();
        copyView.limit(sourceOffset + length);
        copyView.position(sourceOffset);
        copyView.get(target, targetOffset, length);
    }

    private boolean targetsMatch(List<DiscoveryAgent.Peer> peers) {
        if (peers.size() != targets.size()) {
            return false;
        }
        for (int i = 0; i < peers.size(); i++) {
            DiscoveryAgent.Peer peer = peers.get(i);
            Target target = targets.get(i);
            if (peer.streamPort != target.port || !peer.host.equals(target.host)) {
                return false;
            }
        }
        return true;
    }

    private static int findStartCode(ByteBuffer data, int offset, int limit) {
        for (int i = offset; i + 2 < limit; i++) {
            if (data.get(i) != 0 || data.get(i + 1) != 0) {
                continue;
            }
            byte third = data.get(i + 2);
            if (third == 1 || (third == 0 && i + 3 < limit && data.get(i + 3) == 1)) {
                return i;
            }
        }
        return -1;
    }

    private static final class Target {
        final String host;
        final int port;
        final DatagramPacket packet;

        Target(String host, int port, DatagramPacket packet) {
            this.host = host;
            this.port = port;
            this.packet = packet;
        }
    }
}
