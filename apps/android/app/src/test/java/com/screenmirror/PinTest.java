package com.screenmirror;

import org.junit.Test;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNotEquals;

public final class PinTest {
    @Test
    public void normalizeAcceptsExactlyFourAsciiDigits() {
        assertEquals("0123", Pin.normalize(" 0123 "));
    }

    @Test(expected = IllegalArgumentException.class)
    public void normalizeRejectsWrongLength() {
        Pin.normalize("123");
    }

    @Test(expected = IllegalArgumentException.class)
    public void normalizeRejectsNonDigits() {
        Pin.normalize("12a4");
    }

    @Test
    public void hashIsStableAndPinSpecific() throws Exception {
        String first = Pin.hash("1234");
        assertEquals(64, first.length());
        assertEquals(first, Pin.hash("1234"));
        assertNotEquals(first, Pin.hash("1235"));
    }
}
