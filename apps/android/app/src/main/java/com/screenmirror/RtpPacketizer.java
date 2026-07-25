package com.screenmirror;

import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.InetAddress;
import java.nio.ByteBuffer;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.atomic.AtomicInteger;

final class RtpPacketizer {
    private static final int PAYLOAD_TYPE = 96;
    private static final int CLOCK_RATE = 90000;
    private static final int MTU = 1200;

    private final AtomicInteger sequence = new AtomicInteger(1);
    private final int ssrc = (int) System.nanoTime();

    void sendH264(ByteBuffer encoded, int size, long presentationTimeUs, List<DiscoveryAgent.Peer> peers) throws Exception {
        byte[] frame = new byte[size];
        encoded.get(frame);
        for (byte[] nal : splitAnnexB(frame)) {
            if (nal.length == 0) {
                continue;
            }
            for (DiscoveryAgent.Peer peer : peers) {
                sendNal(nal, presentationTimeUs, peer);
            }
        }
    }

    private void sendNal(byte[] nal, long presentationTimeUs, DiscoveryAgent.Peer peer) throws Exception {
        int timestamp = (int) ((presentationTimeUs * CLOCK_RATE) / 1_000_000L);
        try (DatagramSocket socket = new DatagramSocket()) {
            InetAddress address = InetAddress.getByName(peer.host);
            if (nal.length <= MTU - 12) {
                byte[] packet = rtpHeader(timestamp, true, nal.length);
                System.arraycopy(nal, 0, packet, 12, nal.length);
                socket.send(new DatagramPacket(packet, packet.length, address, peer.streamPort));
                return;
            }

            int nalHeader = nal[0] & 0xff;
            int fuIndicator = (nalHeader & 0xe0) | 28;
            int nalType = nalHeader & 0x1f;
            int offset = 1;
            boolean start = true;
            while (offset < nal.length) {
                int payload = Math.min(MTU - 14, nal.length - offset);
                boolean end = offset + payload >= nal.length;
                byte[] packet = rtpHeader(timestamp, end, payload + 2);
                packet[12] = (byte) fuIndicator;
                packet[13] = (byte) ((start ? 0x80 : 0) | (end ? 0x40 : 0) | nalType);
                System.arraycopy(nal, offset, packet, 14, payload);
                socket.send(new DatagramPacket(packet, packet.length, address, peer.streamPort));
                offset += payload;
                start = false;
            }
        }
    }

    private byte[] rtpHeader(int timestamp, boolean marker, int payloadSize) {
        byte[] packet = new byte[12 + payloadSize];
        int seq = sequence.getAndIncrement() & 0xffff;
        packet[0] = (byte) 0x80;
        packet[1] = (byte) (PAYLOAD_TYPE | (marker ? 0x80 : 0));
        packet[2] = (byte) (seq >> 8);
        packet[3] = (byte) seq;
        packet[4] = (byte) (timestamp >> 24);
        packet[5] = (byte) (timestamp >> 16);
        packet[6] = (byte) (timestamp >> 8);
        packet[7] = (byte) timestamp;
        packet[8] = (byte) (ssrc >> 24);
        packet[9] = (byte) (ssrc >> 16);
        packet[10] = (byte) (ssrc >> 8);
        packet[11] = (byte) ssrc;
        return packet;
    }

    private static List<byte[]> splitAnnexB(byte[] frame) {
        ArrayList<byte[]> nals = new ArrayList<>();
        int start = findStartCode(frame, 0);
        while (start >= 0) {
            int nalStart = start + startCodeLength(frame, start);
            int next = findStartCode(frame, nalStart);
            int nalEnd = next >= 0 ? next : frame.length;
            byte[] nal = new byte[Math.max(0, nalEnd - nalStart)];
            System.arraycopy(frame, nalStart, nal, 0, nal.length);
            nals.add(nal);
            start = next;
        }
        if (nals.isEmpty()) {
            nals.add(frame);
        }
        return nals;
    }

    private static int findStartCode(byte[] data, int offset) {
        for (int i = offset; i + 3 < data.length; i++) {
            if (data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1) {
                return i;
            }
            if (i + 4 < data.length && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 0 && data[i + 3] == 1) {
                return i;
            }
        }
        return -1;
    }

    private static int startCodeLength(byte[] data, int offset) {
        return data[offset + 2] == 1 ? 3 : 4;
    }
}
