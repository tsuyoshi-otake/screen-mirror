package com.screenmirror;

import org.junit.Test;

import static org.junit.Assert.assertEquals;

public final class RtpOpusReceiverTest {
    @Test
    public void payloadOffsetHandlesCsrcAndExtension() {
        byte[] packet = new byte[32];
        packet[0] = (byte) 0x91;
        packet[16 + 2] = 0;
        packet[16 + 3] = 2;

        assertEquals(28, RtpOpusReceiver.payloadOffset(packet, packet.length));
    }

    @Test
    public void payloadOffsetRejectsTruncatedExtension() {
        byte[] packet = new byte[16];
        packet[0] = (byte) 0x90;
        packet[14] = 0;
        packet[15] = 1;

        assertEquals(-1, RtpOpusReceiver.payloadOffset(packet, packet.length));
    }

    @Test
    public void readIntUsesNetworkByteOrder() {
        byte[] packet = new byte[]{0x01, 0x23, 0x45, 0x67};
        assertEquals(0x01234567, RtpOpusReceiver.readInt(packet, 0));
    }

    @Test
    public void gainStaysWithinTheSupportedRange() {
        assertEquals(0f, RtpOpusReceiver.clampGain(-1f), 0.0001f);
        assertEquals(0f, RtpOpusReceiver.clampGain(Float.NaN), 0.0001f);
        assertEquals(1.5f, RtpOpusReceiver.clampGain(1.5f), 0.0001f);
        assertEquals(RtpOpusReceiver.MAX_GAIN, RtpOpusReceiver.clampGain(9f), 0.0001f);
    }

    @Test
    public void gainAboveUnityMapsToMillibels() {
        assertEquals(0, RtpOpusReceiver.gainToMillibels(0.5f));
        assertEquals(0, RtpOpusReceiver.gainToMillibels(1f));
        assertEquals(602, RtpOpusReceiver.gainToMillibels(2f));
        assertEquals(1204, RtpOpusReceiver.gainToMillibels(4f));
    }

    @Test
    public void lowLatencyOutputStartsWithTenMilliseconds() {
        assertEquals(480, RtpOpusReceiver.audioFramesForMilliseconds(10));
        assertEquals(1920, RtpOpusReceiver.audioBytesForMilliseconds(10));
    }
}
