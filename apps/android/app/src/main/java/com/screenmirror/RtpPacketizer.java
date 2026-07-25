package com.screenmirror;

import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.InetAddress;
import java.nio.ByteBuffer;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.atomic.AtomicInteger;

final class RtpPacketizer implements AutoCloseable {
    private static final int PAYLOAD_TYPE = 96;
    private static final int CLOCK_RATE = 90000;
    private static final int MTU = 1200;
    private static final int RTP_HEADER_SIZE = 12;
    private static final int FU_A_HEADER_SIZE = 2;

    private final AtomicInteger sequence = new AtomicInteger(1);
    private final int ssrc = (int) System.nanoTime();
    private final byte[] packetBuffer = new byte[MTU];
    private final ArrayList<Target> targets = new ArrayList<>(3);
    private String targetKey = "";
    private DatagramSocket socket;

    void sendH264(ByteBuffer encoded, int size, long presentationTimeUs, List<DiscoveryAgent.Peer> peers) throws Exception {
        if (peers.isEmpty() || size <= 0) {
            return;
        }
        refreshTargets(peers);
        if (targets.isEmpty()) {
            return;
        }

        ByteBuffer frame = encoded.slice();
        frame.limit(size);
        int timestamp = (int) ((presentationTimeUs * CLOCK_RATE) / 1_000_000L);
        int startCode = findStartCode(frame, 0, size);

        if (startCode < 0) {
            sendNal(frame, 0, size, timestamp);
            return;
        }

        while (startCode >= 0) {
            int nalStart = startCode + startCodeLength(frame, startCode);
            int nextStart = findStartCode(frame, nalStart, size);
            int nalEnd = nextStart >= 0 ? nextStart : size;
            if (nalEnd > nalStart) {
                sendNal(frame, nalStart, nalEnd - nalStart, timestamp);
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
        targetKey = "";
    }

    private void sendNal(ByteBuffer frame, int nalOffset, int nalLength, int timestamp) throws Exception {
        if (nalLength <= MTU - RTP_HEADER_SIZE) {
            writeRtpHeader(timestamp, true);
            copyFromBuffer(frame, nalOffset, packetBuffer, RTP_HEADER_SIZE, nalLength);
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
            int payload = Math.min(MTU - RTP_HEADER_SIZE - FU_A_HEADER_SIZE, remaining);
            boolean end = payload == remaining;
            writeRtpHeader(timestamp, end);
            packetBuffer[RTP_HEADER_SIZE] = (byte) fuIndicator;
            packetBuffer[RTP_HEADER_SIZE + 1] = (byte) ((start ? 0x80 : 0) | (end ? 0x40 : 0) | nalType);
            copyFromBuffer(frame, offset, packetBuffer, RTP_HEADER_SIZE + FU_A_HEADER_SIZE, payload);
            sendPacketToTargets(RTP_HEADER_SIZE + FU_A_HEADER_SIZE + payload);
            offset += payload;
            remaining -= payload;
            start = false;
        }
    }

    private void sendPacketToTargets(int packetLength) throws Exception {
        DatagramSocket activeSocket = socket();
        for (int i = 0; i < targets.size(); i++) {
            DatagramPacket packet = targets.get(i).packet;
            packet.setData(packetBuffer, 0, packetLength);
            activeSocket.send(packet);
        }
    }

    private DatagramSocket socket() throws Exception {
        if (socket == null || socket.isClosed()) {
            socket = new DatagramSocket();
            socket.setSendBufferSize(2 * 1024 * 1024);
        }
        return socket;
    }

    private void writeRtpHeader(int timestamp, boolean marker) {
        int seq = sequence.getAndIncrement() & 0xffff;
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
        String key = targetKey(peers);
        if (key.equals(targetKey)) {
            return;
        }

        targets.clear();
        for (int i = 0; i < peers.size(); i++) {
            DiscoveryAgent.Peer peer = peers.get(i);
            InetAddress address = InetAddress.getByName(peer.host);
            targets.add(new Target(new DatagramPacket(packetBuffer, 0, address, peer.streamPort)));
        }
        targetKey = key;
    }

    private static String targetKey(List<DiscoveryAgent.Peer> peers) {
        StringBuilder builder = new StringBuilder(peers.size() * 24);
        for (int i = 0; i < peers.size(); i++) {
            DiscoveryAgent.Peer peer = peers.get(i);
            builder.append(peer.host).append(':').append(peer.streamPort).append(';');
        }
        return builder.toString();
    }

    private static void copyFromBuffer(ByteBuffer source, int sourceOffset, byte[] target, int targetOffset, int length) {
        ByteBuffer duplicate = source.duplicate();
        duplicate.position(sourceOffset);
        duplicate.limit(sourceOffset + length);
        duplicate.get(target, targetOffset, length);
    }

    private static int findStartCode(ByteBuffer data, int offset, int limit) {
        for (int i = offset; i + 3 < limit; i++) {
            if (data.get(i) == 0 && data.get(i + 1) == 0 && data.get(i + 2) == 1) {
                return i;
            }
            if (i + 4 < limit && data.get(i) == 0 && data.get(i + 1) == 0 && data.get(i + 2) == 0 && data.get(i + 3) == 1) {
                return i;
            }
        }
        return -1;
    }

    private static int startCodeLength(ByteBuffer data, int offset) {
        return data.get(offset + 2) == 1 ? 3 : 4;
    }

    private static final class Target {
        final DatagramPacket packet;

        Target(DatagramPacket packet) {
            this.packet = packet;
        }
    }
}
