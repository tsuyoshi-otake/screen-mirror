package com.screenmirror;

import org.junit.Test;

import java.util.Collections;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertTrue;

public final class ScreenSenderProfileTest {
    @Test
    public void peerRefreshRateIsCappedForBalancedGpuLoad() {
        DiscoveryAgent.Peer peer = new DiscoveryAgent.Peer(
                "id",
                "receiver",
                "receiver",
                "192.0.2.1",
                5004,
                5005,
                1920,
                1080,
                120
        );

        ScreenSender.SenderProfile profile =
                ScreenSender.SenderProfile.fromPeers(Collections.singletonList(peer));

        assertEquals(1920, profile.width);
        assertEquals(1080, profile.height);
        assertEquals(30, profile.fps);
        assertTrue(profile.bitrate <= 10_000_000);
    }
}
