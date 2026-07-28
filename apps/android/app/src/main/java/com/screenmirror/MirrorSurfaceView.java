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

    MirrorSurfaceView(Context context) {
        super(context);
    }

    void setVideoSize(int width, int height) {
        if (width <= 0 || height <= 0 || (width == videoWidth && height == videoHeight)) {
            return;
        }
        videoWidth = width;
        videoHeight = height;
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
        long scaledHeight = (long) availableWidth * videoHeight / videoWidth;
        if (scaledHeight <= availableHeight) {
            setMeasuredDimension(availableWidth, (int) scaledHeight);
        } else {
            setMeasuredDimension((int) ((long) availableHeight * videoWidth / videoHeight), availableHeight);
        }
    }
}
