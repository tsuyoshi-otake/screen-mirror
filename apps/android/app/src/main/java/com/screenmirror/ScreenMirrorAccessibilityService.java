package com.screenmirror;

import android.accessibilityservice.AccessibilityService;
import android.accessibilityservice.GestureDescription;
import android.graphics.Point;
import android.graphics.PointF;
import android.graphics.Path;
import android.os.Handler;
import android.os.Looper;
import android.view.Display;
import android.view.WindowManager;
import android.view.accessibility.AccessibilityEvent;

import java.util.ArrayList;
import java.util.HashMap;
import java.util.Map;
import java.util.concurrent.atomic.AtomicReference;

public final class ScreenMirrorAccessibilityService extends AccessibilityService {
    private static final AtomicReference<ScreenMirrorAccessibilityService> ACTIVE = new AtomicReference<>();
    private static final Handler MAIN = new Handler(Looper.getMainLooper());
    private static final long TAP_DURATION_MS = 40;
    private static final long MIN_SWIPE_DURATION_MS = 80;
    private static final long MAX_SWIPE_DURATION_MS = 600;
    private static final int MAX_PATH_POINTS = 64;

    private final Map<Integer, ArrayList<PointF>> activePaths = new HashMap<>();

    @Override
    protected void onServiceConnected() {
        ACTIVE.set(this);
    }

    @Override
    public void onAccessibilityEvent(AccessibilityEvent event) {
    }

    @Override
    public void onInterrupt() {
    }

    @Override
    public boolean onUnbind(android.content.Intent intent) {
        ACTIVE.compareAndSet(this, null);
        return super.onUnbind(intent);
    }

    @Override
    public void onDestroy() {
        ACTIVE.compareAndSet(this, null);
        activePaths.clear();
        super.onDestroy();
    }

    static boolean isConnected() {
        return ACTIVE.get() != null;
    }

    static void dispatchTouch(TouchControlServer.TouchEvent event) {
        ScreenMirrorAccessibilityService service = ACTIVE.get();
        if (service == null) {
            return;
        }
        MAIN.post(() -> service.dispatch(event));
    }

    private void dispatch(TouchControlServer.TouchEvent event) {
        if ("down".equals(event.action)) {
            ArrayList<PointF> path = new ArrayList<>();
            path.add(new PointF(event.x, event.y));
            activePaths.put(event.pointerId, path);
            return;
        }
        if ("move".equals(event.action)) {
            ArrayList<PointF> path = activePaths.get(event.pointerId);
            if (path == null) {
                path = new ArrayList<>();
                path.add(new PointF(event.x, event.y));
                activePaths.put(event.pointerId, path);
            } else {
                appendPoint(path, event.x, event.y);
            }
            return;
        }
        if ("cancel".equals(event.action)) {
            activePaths.remove(event.pointerId);
            return;
        }

        ArrayList<PointF> path = activePaths.remove(event.pointerId);
        if (path == null) {
            path = new ArrayList<>();
        }
        appendPoint(path, event.x, event.y);
        dispatchPath(path);
    }

    private void dispatchPath(ArrayList<PointF> points) {
        if (points.isEmpty()) {
            return;
        }
        Point displaySize = displaySize();
        Path path = new Path();
        PointF first = points.get(0);
        path.moveTo(toScreenX(first.x, displaySize.x), toScreenY(first.y, displaySize.y));
        for (int i = 1; i < points.size(); i++) {
            PointF point = points.get(i);
            path.lineTo(toScreenX(point.x, displaySize.x), toScreenY(point.y, displaySize.y));
        }
        long duration = gestureDuration(points, displaySize);
        GestureDescription gesture = new GestureDescription.Builder()
                .addStroke(new GestureDescription.StrokeDescription(path, 0, duration))
                .build();
        dispatchGesture(gesture, null, null);
    }

    private Point displaySize() {
        Point size = new Point(1, 1);
        Object windowService = getSystemService(WINDOW_SERVICE);
        if (windowService instanceof WindowManager) {
            Display display = ((WindowManager) windowService).getDefaultDisplay();
            display.getRealSize(size);
        }
        size.x = Math.max(1, size.x);
        size.y = Math.max(1, size.y);
        return size;
    }

    private static void appendPoint(ArrayList<PointF> points, float x, float y) {
        if (points.size() >= MAX_PATH_POINTS) {
            points.remove(1);
        }
        points.add(new PointF(x, y));
    }

    private static long gestureDuration(ArrayList<PointF> points, Point displaySize) {
        if (points.size() <= 1 || distancePx(points, displaySize) < 8.0f) {
            return TAP_DURATION_MS;
        }
        long duration = Math.max(MIN_SWIPE_DURATION_MS, points.size() * 16L);
        return Math.min(MAX_SWIPE_DURATION_MS, duration);
    }

    private static float distancePx(ArrayList<PointF> points, Point displaySize) {
        PointF first = points.get(0);
        PointF last = points.get(points.size() - 1);
        float dx = toScreenX(last.x, displaySize.x) - toScreenX(first.x, displaySize.x);
        float dy = toScreenY(last.y, displaySize.y) - toScreenY(first.y, displaySize.y);
        return (float) Math.sqrt(dx * dx + dy * dy);
    }

    private static float toScreenX(float x, int width) {
        return x * Math.max(0, width - 1);
    }

    private static float toScreenY(float y, int height) {
        return y * Math.max(0, height - 1);
    }
}
