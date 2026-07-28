package com.screenmirror;

import org.junit.Test;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

/**
 * Depacketization runs without a decoder attached, so these exercise the parsing rules only. The
 * sender aggregates its parameter sets into STAP-A, which the receiver used to drop outright.
 */
public final class RtpH264ReceiverTest {
    private final RtpH264Receiver receiver = new RtpH264Receiver();

    @Test
    public void aggregatedParameterSetsAreAccepted() throws Exception {
        byte[] sps = new byte[]{0x67, 0x42, 0x00, 0x1f};
        byte[] pps = new byte[]{0x68, (byte) 0xce, 0x3c, (byte) 0x80};
        byte[] packet = stapA(sps, pps);

        assertTrue(receiver.depacketizeAndQueue(packet, packet.length));
    }

    @Test
    public void aggregationWithATruncatedLengthIsRejected() throws Exception {
        byte[] packet = new byte[16];
        packet[0] = (byte) 0x80;
        packet[1] = 96;
        packet[12] = 24;
        packet[13] = 0x00;
        packet[14] = (byte) 0xff; // Claims 255 bytes that the packet does not carry.

        assertFalse(receiver.depacketizeAndQueue(packet, 15));
    }

    @Test
    public void singleNalPacketsStillWork() throws Exception {
        byte[] packet = new byte[20];
        packet[0] = (byte) 0x80;
        packet[1] = 96;
        packet[12] = 0x65; // IDR slice

        assertTrue(receiver.depacketizeAndQueue(packet, packet.length));
    }

    @Test
    public void receiverLocksToFirstSenderHostAndSsrc() {
        byte[] first = singleNal(0x10203040);
        byte[] same = singleNal(0x10203040);
        byte[] otherSsrc = singleNal(0x10203041);

        assertTrue(receiver.acceptsStreamPacket("192.168.1.10", first, first.length));
        assertTrue(receiver.acceptsStreamPacket("192.168.1.10", same, same.length));
        assertFalse(receiver.acceptsStreamPacket("192.168.1.11", same, same.length));
        assertFalse(receiver.acceptsStreamPacket("192.168.1.10", otherSsrc, otherSsrc.length));
    }

    @Test
    public void stoppingReceiverReleasesSenderLock() {
        byte[] first = singleNal(1);
        byte[] replacement = singleNal(2);

        assertTrue(receiver.acceptsStreamPacket("192.168.1.10", first, first.length));
        receiver.stop();
        assertTrue(receiver.acceptsStreamPacket("192.168.1.11", replacement, replacement.length));
    }

    @Test
    public void rejectedTrafficCannotKeepDeadSelectedStreamAliveForever() {
        FakeClock clock = new FakeClock();
        RtpH264Receiver timedReceiver = new RtpH264Receiver(new DisconnectWatchdog(3000, clock));
        byte[] original = singleNal(1);
        byte[] replacement = singleNal(2);

        assertTrue(timedReceiver.acceptsStreamPacket("192.168.1.10", original, original.length));
        clock.advanceMs(2999);
        assertFalse(timedReceiver.acceptsStreamPacket("192.168.1.11", replacement, replacement.length));
        clock.advanceMs(1);
        assertTrue(timedReceiver.acceptsStreamPacket("192.168.1.11", replacement, replacement.length));
        assertFalse(timedReceiver.acceptsStreamPacket("192.168.1.10", original, original.length));
    }

    @Test
    public void invalidRtpVersionAndPayloadTypeAreRejected() throws Exception {
        byte[] packet = singleNal(1);
        packet[0] = 0x40; // RTP version 1.
        assertFalse(receiver.depacketizeAndQueue(packet, packet.length));
        assertFalse(receiver.acceptsStreamPacket("192.168.1.10", packet, packet.length));

        packet = singleNal(1);
        packet[1] = 97; // Dynamic payload type other than the sender's H.264 type 96.
        assertFalse(receiver.depacketizeAndQueue(packet, packet.length));
        assertFalse(receiver.acceptsStreamPacket("192.168.1.10", packet, packet.length));
        byte[] valid = singleNal(2);
        assertTrue(receiver.acceptsStreamPacket("192.168.1.11", valid, valid.length));
    }

    @Test
    public void rtpHeaderExtensionIsSkippedBeforeH264Payload() throws Exception {
        byte[] packet = new byte[17];
        packet[0] = (byte) 0x90; // RTP v2 plus an extension.
        packet[1] = 96;
        packet[14] = 0x00;
        packet[15] = 0x00; // Extension length is zero 32-bit words.
        packet[16] = 0x65; // IDR payload follows the extension header.

        assertTrue(receiver.depacketizeAndQueue(packet, packet.length));
    }

    @Test
    public void sequenceGapDoesNotPauseFollowingFrames() throws Exception {
        assertTrue(receiver.depacketizeAndQueue(h264Packet(10, 100, (byte) 0x65), 13));
        // A loss in an unrelated single-NAL packet must not freeze the receiver until the next
        // key frame. Fragmented NALs retain their own stricter sequence guard below.
        assertTrue(receiver.depacketizeAndQueue(h264Packet(12, 100, (byte) 0x61), 13));
        assertTrue(receiver.depacketizeAndQueue(h264Packet(14, 200, (byte) 0x61), 13));
    }

    @Test
    public void missingFuFragmentCannotCompleteNalOrPoisonNextFrame() throws Exception {
        assertFalse(receiver.depacketizeAndQueue(fuPacket(20, 500, true, false, (byte) 0x11), 15));
        // Sequence 21 is absent. Its end fragment must not be emitted as a damaged IDR NAL.
        assertFalse(receiver.depacketizeAndQueue(fuPacket(22, 500, false, true, (byte) 0x22), 15));

        // A fresh FU-A starts a clean NAL immediately; no key-frame wait is introduced.
        assertFalse(receiver.depacketizeAndQueue(fuPacket(23, 600, true, false, (byte) 0x33), 15));
        assertTrue(receiver.depacketizeAndQueue(fuPacket(24, 600, false, true, (byte) 0x44), 15));
    }

    @Test
    public void fragmentedNalSequenceCanWrap() throws Exception {
        assertFalse(receiver.depacketizeAndQueue(fuPacket(0xffff, 700, true, false, (byte) 0x11), 15));
        assertTrue(receiver.depacketizeAndQueue(fuPacket(0, 700, false, true, (byte) 0x22), 15));
    }

    private static byte[] stapA(byte[]... nals) {
        int payloadLength = 1;
        for (byte[] nal : nals) {
            payloadLength += 2 + nal.length;
        }
        byte[] packet = new byte[12 + payloadLength];
        packet[0] = (byte) 0x80;
        packet[1] = 96;
        packet[12] = 24;
        int cursor = 13;
        for (byte[] nal : nals) {
            packet[cursor++] = (byte) (nal.length >> 8);
            packet[cursor++] = (byte) nal.length;
            System.arraycopy(nal, 0, packet, cursor, nal.length);
            cursor += nal.length;
        }
        return packet;
    }

    private static byte[] singleNal(int ssrc) {
        return h264Packet(0, 0, ssrc, (byte) 0x65);
    }

    private static byte[] h264Packet(int sequence, int timestamp, byte nalHeader) {
        return h264Packet(sequence, timestamp, 0, nalHeader);
    }

    private static byte[] h264Packet(int sequence, int timestamp, int ssrc, byte nalHeader) {
        byte[] packet = new byte[13];
        packet[0] = (byte) 0x80;
        packet[1] = 96;
        packet[2] = (byte) (sequence >> 8);
        packet[3] = (byte) sequence;
        packet[4] = (byte) (timestamp >> 24);
        packet[5] = (byte) (timestamp >> 16);
        packet[6] = (byte) (timestamp >> 8);
        packet[7] = (byte) timestamp;
        packet[8] = (byte) (ssrc >> 24);
        packet[9] = (byte) (ssrc >> 16);
        packet[10] = (byte) (ssrc >> 8);
        packet[11] = (byte) ssrc;
        packet[12] = nalHeader;
        return packet;
    }

    private static byte[] fuPacket(int sequence, int timestamp, boolean start, boolean end, byte payload) {
        byte[] packet = new byte[15];
        packet[0] = (byte) 0x80;
        packet[1] = 96;
        packet[2] = (byte) (sequence >> 8);
        packet[3] = (byte) sequence;
        packet[4] = (byte) (timestamp >> 24);
        packet[5] = (byte) (timestamp >> 16);
        packet[6] = (byte) (timestamp >> 8);
        packet[7] = (byte) timestamp;
        packet[12] = 0x7c; // FU-A carrying an IDR NAL.
        packet[13] = (byte) ((start ? 0x80 : 0) | (end ? 0x40 : 0) | 5);
        packet[14] = payload;
        return packet;
    }

    private static final class FakeClock implements DisconnectWatchdog.Clock {
        private long nowNanos;

        @Override
        public long nanoTime() {
            return nowNanos;
        }

        void advanceMs(long milliseconds) {
            nowNanos += milliseconds * 1_000_000L;
        }
    }
}
