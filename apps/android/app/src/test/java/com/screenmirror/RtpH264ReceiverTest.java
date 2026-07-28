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
}
