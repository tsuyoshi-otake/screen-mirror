package com.screenmirror;

final class DisconnectWatchdog {
    interface Clock {
        long nanoTime();
    }

    private final long timeoutNanos;
    private final Clock clock;
    private long lastPacketNanos = -1L;

    DisconnectWatchdog(long timeoutMs) {
        this(timeoutMs, System::nanoTime);
    }

    DisconnectWatchdog(long timeoutMs, Clock clock) {
        if (timeoutMs <= 0) {
            throw new IllegalArgumentException("timeout must be positive");
        }
        this.timeoutNanos = timeoutMs * 1_000_000L;
        this.clock = clock;
    }

    boolean onPacket() {
        boolean firstPacket = lastPacketNanos < 0;
        lastPacketNanos = clock.nanoTime();
        return firstPacket;
    }

    boolean hasStarted() {
        return lastPacketNanos >= 0;
    }

    boolean isTimedOut() {
        return hasStarted() && clock.nanoTime() - lastPacketNanos >= timeoutNanos;
    }

    void reset() {
        lastPacketNanos = -1L;
    }
}
