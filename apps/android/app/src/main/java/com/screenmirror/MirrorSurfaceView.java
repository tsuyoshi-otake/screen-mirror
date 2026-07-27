package com.screenmirror;

import android.content.Context;
import android.view.SurfaceView;

final class MirrorSurfaceView extends SurfaceView {
    MirrorSurfaceView(Context context) {
        super(context);
    }

    @Override
    public boolean performClick() {
        super.performClick();
        return true;
    }
}
