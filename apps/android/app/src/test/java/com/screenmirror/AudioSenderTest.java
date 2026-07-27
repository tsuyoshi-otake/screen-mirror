package com.screenmirror;

import org.junit.Test;

import static org.junit.Assert.assertEquals;

public final class AudioSenderTest {
    @Test
    public void captureChunkIsOneTwoPointFiveMillisecondOpusFrame() {
        assertEquals(120, AudioSender.FRAME_SAMPLES);
        assertEquals(480, AudioSender.PCM_FRAME_BYTES);
    }
}
