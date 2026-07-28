package com.screenmirror;

import android.content.Context;
import android.view.SurfaceView;

/**
 * A surface that keeps the sender's aspect ratio. Without this the codec output is stretched to
 * whatever shape the view happens to have, which turns a wide desktop into a distorted portrait.
 */
final class MirrorSurfaceView extends SurfaceView {
    private int videoWidth;
    private int videoHeight;
    private int bufferWidth;
    private int bufferHeight;

    MirrorSurfaceView(Context context) {
        super(context);
    }

    /**
     * The view is measured using the visible crop, while the Surface buffer uses the decoder's
     * coded dimensions.  They differ for e.g. 1088-line H.264 frames cropped to 1080 lines.
     * Keeping both values avoids the compositor stretching a new decoder format into an old
     * Surface buffer after an SPS or rotation change.
     */
    void setVideoSize(int width, int height, int codedWidth, int codedHeight) {
        if (width <= 0 || height <= 0) {
            return;
        }
        int nextBufferWidth = codedWidth > 0 ? codedWidth : width;
        int nextBufferHeight = codedHeight > 0 ? codedHeight : height;
        boolean viewChanged = width != videoWidth || height != videoHeight;
        boolean bufferChanged = nextBufferWidth != bufferWidth || nextBufferHeight != bufferHeight;
        if (!viewChanged && !bufferChanged) {
            return;
        }
        videoWidth = width;
        videoHeight = height;
        bufferWidth = nextBufferWidth;
        bufferHeight = nextBufferHeight;
        if (bufferChanged) {
            getHolder().setFixedSize(bufferWidth, bufferHeight);
        }
        if (viewChanged) {
            requestLayout();
        }
    }

    void clearVideoSize() {
        boolean hadFixedBuffer = bufferWidth > 0 || bufferHeight > 0;
        videoWidth = 0;
        videoHeight = 0;
        bufferWidth = 0;
        bufferHeight = 0;
        if (hadFixedBuffer) {
            getHolder().setSizeFromLayout();
        }
        requestLayout();
    }

    @Override
    protected void onMeasure(int widthMeasureSpec, int heightMeasureSpec) {
        int availableWidth = getDefaultSize(getSuggestedMinimumWidth(), widthMeasureSpec);
        int availableHeight = getDefaultSize(getSuggestedMinimumHeight(), heightMeasureSpec);
        if (videoWidth <= 0 || videoHeight <= 0 || availableWidth <= 0 || availableHeight <= 0) {
            setMeasuredDimension(availableWidth, availableHeight);
            return;
        }

        // Fit inside the space we were offered; the leftover strip stays black.
        VideoPresentation.Size fitted = VideoPresentation.fitInside(
                availableWidth, availableHeight, videoWidth, videoHeight
        );
        setMeasuredDimension(fitted.width, fitted.height);
    }
}
