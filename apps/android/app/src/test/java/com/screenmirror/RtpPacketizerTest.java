package com.screenmirror;

import org.junit.Test;

import java.util.Random;

import static org.junit.Assert.assertEquals;

/**
 * The start-code scan skips three bytes at a time instead of walking one, which is only safe if it
 * agrees with the naive scan on every boundary. These pin that down: a disagreement would move a NAL
 * boundary and corrupt the stream rather than fail visibly.
 */
public final class RtpPacketizerTest {
    @Test
    public void findsBothStartCodeFormsAtTheirFirstByte() {
        assertEquals(0, RtpPacketizer.findStartCode(bytes(0, 0, 1, 0x65), 0, 4));
        assertEquals(0, RtpPacketizer.findStartCode(bytes(0, 0, 0, 1, 0x65), 0, 5));
        assertEquals(2, RtpPacketizer.findStartCode(bytes(0x65, 0x42, 0, 0, 1, 0x67), 0, 6));
        // A four-byte code has a three-byte one inside it; the first byte is what must be reported.
        assertEquals(1, RtpPacketizer.findStartCode(bytes(0x09, 0, 0, 0, 1, 0x65), 0, 6));
    }

    @Test
    public void findsNothingWithoutACompleteStartCode() {
        assertEquals(-1, RtpPacketizer.findStartCode(bytes(0x65, 0x42, 0x11), 0, 3));
        assertEquals(-1, RtpPacketizer.findStartCode(bytes(0, 0, 2), 0, 3));
        assertEquals(-1, RtpPacketizer.findStartCode(bytes(0, 0), 0, 2));
        // The trailing 1 lies past the limit, so the code is not complete within it.
        assertEquals(-1, RtpPacketizer.findStartCode(bytes(0x65, 0, 0, 1), 0, 3));
    }

    @Test
    public void skipsPastARunOfZeros() {
        // Five zeros then a 1: the four-byte code begins at the third zero, not the first.
        assertEquals(2, RtpPacketizer.findStartCode(bytes(0, 0, 0, 0, 0, 1, 0x65), 0, 7));
        assertEquals(-1, RtpPacketizer.findStartCode(bytes(0, 0, 0, 0, 0, 0), 0, 6));
    }

    @Test
    public void resumesFromAnOffsetWithoutRefindingTheCodeBehindIt() {
        byte[] frame = bytes(0, 0, 1, 0x67, 0, 0, 1, 0x65);
        assertEquals(0, RtpPacketizer.findStartCode(frame, 0, frame.length));
        assertEquals(4, RtpPacketizer.findStartCode(frame, 3, frame.length));
        assertEquals(-1, RtpPacketizer.findStartCode(frame, 7, frame.length));
    }

    /**
     * Every arrangement of the only three byte values that matter, at every offset: the skipping
     * scan has no room to differ from the naive one anywhere in that space.
     */
    @Test
    public void agreesWithTheNaiveScanOnEveryShortPattern() {
        for (int length = 0; length <= 6; length++) {
            byte[] data = new byte[length];
            int combinations = (int) Math.pow(3, length);
            for (int combination = 0; combination < combinations; combination++) {
                int remaining = combination;
                for (int i = 0; i < length; i++) {
                    data[i] = (byte) (remaining % 3);
                    remaining /= 3;
                }
                for (int offset = 0; offset <= length; offset++) {
                    assertEquals(
                            describe(data, offset),
                            naiveFindStartCode(data, offset, length),
                            RtpPacketizer.findStartCode(data, offset, length));
                }
            }
        }
    }

    @Test
    public void agreesWithTheNaiveScanOnLongerFrames() {
        Random random = new Random(20260730L);
        for (int trial = 0; trial < 2000; trial++) {
            byte[] data = new byte[1 + random.nextInt(64)];
            for (int i = 0; i < data.length; i++) {
                // Zero-heavy, so start codes and near misses both occur often.
                data[i] = (byte) (random.nextInt(4) == 0 ? random.nextInt(256) : random.nextInt(2));
            }
            int offset = random.nextInt(data.length);
            assertEquals(
                    describe(data, offset),
                    naiveFindStartCode(data, offset, data.length),
                    RtpPacketizer.findStartCode(data, offset, data.length));
        }
    }

    /** The one-byte-at-a-time scan this replaced, kept as the reference definition. */
    private static int naiveFindStartCode(byte[] data, int offset, int limit) {
        for (int i = offset; i + 2 < limit; i++) {
            if (data[i] != 0 || data[i + 1] != 0) {
                continue;
            }
            byte third = data[i + 2];
            if (third == 1 || (third == 0 && i + 3 < limit && data[i + 3] == 1)) {
                return i;
            }
        }
        return -1;
    }

    private static String describe(byte[] data, int offset) {
        StringBuilder text = new StringBuilder("offset ").append(offset).append(" of");
        for (byte value : data) {
            text.append(' ').append(value & 0xff);
        }
        return text.toString();
    }

    private static byte[] bytes(int... values) {
        byte[] data = new byte[values.length];
        for (int i = 0; i < values.length; i++) {
            data[i] = (byte) values[i];
        }
        return data;
    }
}
