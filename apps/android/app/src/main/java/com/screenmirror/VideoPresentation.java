package com.screenmirror;

/**
 * Geometry decisions shared by the receiver activity and its preview surface.  Keeping this free
 * of Android framework types lets the aspect-ratio rules be tested on the JVM.
 */
final class VideoPresentation {
    enum Orientation {
        UNSPECIFIED,
        LANDSCAPE,
        PORTRAIT
    }

    private VideoPresentation() {
    }

    /**
     * Do not react to nearly-square (or transient malformed) formats.  A decoder can briefly
     * report its coded dimensions before the crop rectangle is available; retaining the previous
     * orientation prevents that intermediate report from rotating the activity back and forth.
     */
    static Orientation orientationFor(int width, int height, Orientation current) {
        if (width <= 0 || height <= 0) {
            return current;
        }
        if ((long) width * 10 >= (long) height * 11) {
            return Orientation.LANDSCAPE;
        }
        if ((long) height * 10 >= (long) width * 11) {
            return Orientation.PORTRAIT;
        }
        return current;
    }

    /** Returns the largest whole-pixel rectangle with {@code videoWidth:videoHeight} in bounds. */
    static Size fitInside(int availableWidth, int availableHeight, int videoWidth, int videoHeight) {
        if (availableWidth <= 0 || availableHeight <= 0
                || videoWidth <= 0 || videoHeight <= 0) {
            return new Size(Math.max(0, availableWidth), Math.max(0, availableHeight));
        }
        long scaledHeight = (long) availableWidth * videoHeight / videoWidth;
        if (scaledHeight <= availableHeight) {
            return new Size(availableWidth, Math.max(1, (int) scaledHeight));
        }
        long scaledWidth = (long) availableHeight * videoWidth / videoHeight;
        return new Size(Math.max(1, (int) scaledWidth), availableHeight);
    }

    static final class Size {
        final int width;
        final int height;

        Size(int width, int height) {
            this.width = width;
            this.height = height;
        }
    }
}
