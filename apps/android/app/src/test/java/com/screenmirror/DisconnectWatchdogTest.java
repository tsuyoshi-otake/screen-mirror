package com.screenmirror;

import org.junit.Test;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

public final class DisconnectWatchdogTest {
    @Test
    public void timeoutIsArmedOnlyAfterFirstPacket() {
        FakeClock clock = new FakeClock();
        DisconnectWatchdog watchdog = new DisconnectWatchdog(3000, clock);

        clock.advanceMs(10_000);
        assertFalse(watchdog.isTimedOut());
        assertTrue(watchdog.onPacket());
        clock.advanceMs(2999);
        assertFalse(watchdog.isTimedOut());
        clock.advanceMs(1);
        assertTrue(watchdog.isTimedOut());
    }

    @Test
    public void eachPacketExtendsDeadlineAndResetDisarmsIt() {
        FakeClock clock = new FakeClock();
        DisconnectWatchdog watchdog = new DisconnectWatchdog(3000, clock);

        watchdog.onPacket();
        clock.advanceMs(2000);
        assertFalse(watchdog.onPacket());
        clock.advanceMs(2000);
        assertFalse(watchdog.isTimedOut());
        watchdog.reset();
        clock.advanceMs(4000);
        assertFalse(watchdog.isTimedOut());
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
