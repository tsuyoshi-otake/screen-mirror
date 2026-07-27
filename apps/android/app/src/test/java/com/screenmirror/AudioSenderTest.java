package com.screenmirror;

import org.junit.Test;

import static org.junit.Assert.assertEquals;

public final class AudioSenderTest {
    @Test
    public void captureChunkIsOneFiveMillisecondOpusFrame() {
        assertEquals(240, AudioSender.FRAME_SAMPLES);
        assertEquals(960, AudioSender.PCM_FRAME_BYTES);
    }
}
