package com.screenmirror;

import org.junit.Test;

import static org.junit.Assert.assertEquals;

public final class VideoPresentationTest {
    @Test
    public void choosesOrientationFromClearlyWideOrTallVideo() {
        assertEquals(VideoPresentation.Orientation.LANDSCAPE,
                VideoPresentation.orientationFor(1920, 1080, VideoPresentation.Orientation.UNSPECIFIED));
        assertEquals(VideoPresentation.Orientation.PORTRAIT,
                VideoPresentation.orientationFor(1080, 1920, VideoPresentation.Orientation.UNSPECIFIED));
    }

    @Test
    public void nearlySquareFormatKeepsCurrentOrientation() {
        assertEquals(VideoPresentation.Orientation.LANDSCAPE,
                VideoPresentation.orientationFor(1080, 1040, VideoPresentation.Orientation.LANDSCAPE));
        assertEquals(VideoPresentation.Orientation.PORTRAIT,
                VideoPresentation.orientationFor(1040, 1080, VideoPresentation.Orientation.PORTRAIT));
        assertEquals(VideoPresentation.Orientation.UNSPECIFIED,
                VideoPresentation.orientationFor(1000, 1000, VideoPresentation.Orientation.UNSPECIFIED));
    }

    @Test
    public void fitsWideAndTallVideoWithoutChangingAspectRatio() {
        VideoPresentation.Size wide = VideoPresentation.fitInside(1080, 1920, 1920, 1080);
        assertEquals(1080, wide.width);
        assertEquals(607, wide.height);

        VideoPresentation.Size tall = VideoPresentation.fitInside(1920, 1080, 1080, 1920);
        assertEquals(607, tall.width);
        assertEquals(1080, tall.height);
    }
}
