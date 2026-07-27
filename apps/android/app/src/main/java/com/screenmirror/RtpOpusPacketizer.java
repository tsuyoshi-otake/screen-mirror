package com.screenmirror;

import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.InetAddress;
import java.nio.ByteBuffer;
import java.util.ArrayList;
import java.util.List;

final class RtpOpusPacketizer implements AutoCloseable {
    private static final int PAYLOAD_TYPE = 97;
    private static final int CLOCK_RATE = 48_000;
    private static final int MTU = 1200;
    private static final int SOCKET_BUFFER_SIZE = 256 * 1024;
    private static final int DSCP_EF_TRAFFIC_CLASS = 0xB8;
    private static final int RTP_HEADER_SIZE = 12;

    private final byte[] packetBuffer = new byte[MTU];
    private final ArrayList<Target> targets = new ArrayList<>(3);
    private final int ssrc = (int) System.nanoTime();

    private DatagramSocket socket;
    private int sequence = 1;
    private boolean targetsInitialized = false;

    void sendOpus(ByteBuffer encoded, int size, long presentationTimeUs, List<DiscoveryAgent.Peer> peers) throws Exception {
        if (peers.isEmpty() || size <= 0 || size > MTU - RTP_HEADER_SIZE) {
            return;
        }
        if (!targetsInitialized || !targetsMatch(peers)) {
            refreshTargets(peers);
        }
        if (targets.isEmpty()) {
            return;
        }

        int timestamp = (int) ((presentationTimeUs * CLOCK_RATE) / 1_000_000L);
        writeRtpHeader(timestamp);
        ByteBuffer copy = encoded.slice();
        copy.limit(size);
        copy.get(packetBuffer, RTP_HEADER_SIZE, size);
        sendPacketToTargets(RTP_HEADER_SIZE + size);
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

    private void sendPacketToTargets(int packetLength) throws Exception {
        DatagramSocket activeSocket = socket();
        for (int index = 0; index < targets.size(); index++) {
            DatagramPacket packet = targets.get(index).packet;
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
                AppLog.warn("audio sender could not set DSCP", error);
            }
        }
        return socket;
    }

    private void writeRtpHeader(int timestamp) {
        int seq = sequence++ & 0xffff;
        packetBuffer[0] = (byte) 0x80;
        packetBuffer[1] = (byte) (PAYLOAD_TYPE | 0x80);
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
        targets.clear();
        for (int index = 0; index < peers.size(); index++) {
            DiscoveryAgent.Peer peer = peers.get(index);
            InetAddress address = InetAddress.getByName(peer.host);
            targets.add(new Target(
                    peer.host,
                    peer.audioPort,
                    new DatagramPacket(packetBuffer, MTU, address, peer.audioPort)
            ));
        }
        targetsInitialized = true;
    }

    private boolean targetsMatch(List<DiscoveryAgent.Peer> peers) {
        if (peers.size() != targets.size()) {
            return false;
        }
        for (int index = 0; index < peers.size(); index++) {
            DiscoveryAgent.Peer peer = peers.get(index);
            Target target = targets.get(index);
            if (peer.audioPort != target.port || !peer.host.equals(target.host)) {
                return false;
            }
        }
        return true;
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
