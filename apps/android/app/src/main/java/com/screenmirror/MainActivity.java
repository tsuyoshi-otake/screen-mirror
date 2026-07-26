package com.screenmirror;

import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.media.projection.MediaProjection;
import android.media.projection.MediaProjectionManager;
import android.net.wifi.WifiManager;
import android.os.Bundle;
import android.util.DisplayMetrics;
import android.view.SurfaceHolder;
import android.view.SurfaceView;
import android.view.MotionEvent;
import android.view.View;
import android.view.WindowManager;
import android.widget.Button;
import android.widget.LinearLayout;
import android.widget.TextView;

import java.util.ArrayList;
import java.util.List;

public final class MainActivity extends Activity {
    private static final int STREAM_PORT = 5004;
    private static final int REQUEST_CAPTURE = 1001;

    private final DiscoveryAgent discovery = new DiscoveryAgent();
    private final RtpH264Receiver receiver = new RtpH264Receiver();
    private final ScreenSender sender = new ScreenSender();
    private final ControlClient control = new ControlClient();
    private final ArrayList<DiscoveryAgent.Peer> selectedReceivers = new ArrayList<>();

    private SurfaceView surfaceView;
    private TextView status;
    private LinearLayout toolbar;
    private MediaProjectionManager projectionManager;
    private WifiManager.MulticastLock multicastLock;

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        projectionManager = (MediaProjectionManager) getSystemService(Context.MEDIA_PROJECTION_SERVICE);
        WifiManager wifi = (WifiManager) getApplicationContext().getSystemService(Context.WIFI_SERVICE);
        multicastLock = wifi.createMulticastLock("screen-mirror-discovery");
        multicastLock.setReferenceCounted(false);

        surfaceView = new SurfaceView(this);
        surfaceView.setOnTouchListener((view, event) -> {
            String host = receiver.lastSenderHost();
            if (host == null || host.isEmpty()) {
                return true;
            }
            String action;
            switch (event.getActionMasked()) {
                case MotionEvent.ACTION_DOWN:
                case MotionEvent.ACTION_POINTER_DOWN:
                    action = "down";
                    break;
                case MotionEvent.ACTION_UP:
                case MotionEvent.ACTION_POINTER_UP:
                    action = "up";
                    break;
                case MotionEvent.ACTION_CANCEL:
                    action = "cancel";
                    break;
                default:
                    action = "move";
                    break;
            }
            int index = event.getActionIndex();
            float x = event.getX(index) / Math.max(1, view.getWidth());
            float y = event.getY(index) / Math.max(1, view.getHeight());
            control.send(host, action, x, y, event.getPointerId(index));
            return true;
        });
        status = new TextView(this);
        status.setText("Status: idle");
        status.setPadding(24, 24, 24, 24);

        Button startReceiver = new Button(this);
        startReceiver.setText("Start Receiver");
        startReceiver.setOnClickListener(view -> startReceiver());

        Button discover = new Button(this);
        discover.setText("Discover Receivers");
        discover.setOnClickListener(view -> discoverReceivers());

        Button startSender = new Button(this);
        startSender.setText("Start Sender (up to 3 receivers)");
        startSender.setOnClickListener(view -> startSender());

        Button stop = new Button(this);
        stop.setText("Stop");
        stop.setOnClickListener(view -> stopAll());

        toolbar = new LinearLayout(this);
        toolbar.setOrientation(LinearLayout.VERTICAL);
        toolbar.addView(status);
        toolbar.addView(startReceiver);
        toolbar.addView(discover);
        toolbar.addView(startSender);
        toolbar.addView(stop);

        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        root.addView(toolbar);
        root.addView(surfaceView, new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT,
                0,
                1.0f
        ));
        setContentView(root);
    }

    @Override
    protected void onDestroy() {
        stopAll();
        super.onDestroy();
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode != REQUEST_CAPTURE || resultCode != RESULT_OK || data == null) {
            setStatus("Status: sender permission denied");
            return;
        }
        try {
            MediaProjection projection = projectionManager.getMediaProjection(resultCode, data);
            sender.start(projection, new ArrayList<>(selectedReceivers));
            setStatus("Status: sending to " + selectedReceivers.size() + " receiver(s)");
        } catch (Exception error) {
            setStatus("Sender failed: " + error.getMessage());
        }
    }

    private void startReceiver() {
        stopAll();
        enterReceiverFullscreen();
        keepReceiverAwake(true);
        SurfaceHolder holder = surfaceView.getHolder();
        holder.addCallback(new SurfaceHolder.Callback() {
            @Override
            public void surfaceCreated(SurfaceHolder holder) {
                try {
                    lockMulticast();
                    discovery.startReceiverBeacon(STREAM_PORT, displayWidth(), displayHeight(), displayRefreshHz());
                    receiver.start(STREAM_PORT, holder.getSurface());
                    setStatus("Status: receiving on :" + STREAM_PORT);
                } catch (Exception error) {
                    setStatus("Receiver failed: " + error.getMessage());
                }
            }

            @Override
            public void surfaceChanged(SurfaceHolder holder, int format, int width, int height) {
            }

            @Override
            public void surfaceDestroyed(SurfaceHolder holder) {
                receiver.stop();
            }
        });
        if (holder.getSurface().isValid()) {
            try {
                lockMulticast();
                discovery.startReceiverBeacon(STREAM_PORT, displayWidth(), displayHeight(), displayRefreshHz());
                receiver.start(STREAM_PORT, holder.getSurface());
                setStatus("Status: receiving on :" + STREAM_PORT);
            } catch (Exception error) {
                setStatus("Receiver failed: " + error.getMessage());
            }
        }
    }

    private void discoverReceivers() {
        lockMulticast();
        setStatus("Status: discovering receivers...");
        new Thread(() -> {
            try {
                List<DiscoveryAgent.Peer> peers = discovery.discoverReceivers(3000);
                selectedReceivers.clear();
                for (int i = 0; i < peers.size() && i < 3; i++) {
                    selectedReceivers.add(peers.get(i));
                }
                runOnUiThread(() -> setStatus("Discovered " + selectedReceivers.size() + " receiver(s): " + selectedReceivers));
            } catch (Exception error) {
                runOnUiThread(() -> setStatus("Discovery failed: " + error.getMessage()));
            }
        }, "discover-receivers").start();
    }

    private void startSender() {
        stopAll();
        lockMulticast();
        new Thread(() -> {
            try {
                if (selectedReceivers.isEmpty()) {
                    List<DiscoveryAgent.Peer> peers = discovery.discoverReceivers(3000);
                    selectedReceivers.clear();
                    for (int i = 0; i < peers.size() && i < 3; i++) {
                        selectedReceivers.add(peers.get(i));
                    }
                }
                if (selectedReceivers.isEmpty()) {
                    runOnUiThread(() -> setStatus("No receivers found"));
                    return;
                }
                runOnUiThread(() -> startActivityForResult(
                        projectionManager.createScreenCaptureIntent(),
                        REQUEST_CAPTURE
                ));
            } catch (Exception error) {
                runOnUiThread(() -> setStatus("Sender setup failed: " + error.getMessage()));
            }
        }, "sender-setup").start();
    }

    private void stopAll() {
        discovery.stop();
        receiver.stop();
        sender.stop();
        leaveReceiverFullscreen();
        keepReceiverAwake(false);
        if (multicastLock != null && multicastLock.isHeld()) {
            multicastLock.release();
        }
        setStatus("Status: idle");
    }

    private void lockMulticast() {
        if (multicastLock != null && !multicastLock.isHeld()) {
            multicastLock.acquire();
        }
    }

    private int displayWidth() {
        DisplayMetrics metrics = getResources().getDisplayMetrics();
        return metrics.widthPixels;
    }

    private int displayHeight() {
        DisplayMetrics metrics = getResources().getDisplayMetrics();
        return metrics.heightPixels;
    }

    private int displayRefreshHz() {
        return Math.round(getWindowManager().getDefaultDisplay().getRefreshRate());
    }

    private void enterReceiverFullscreen() {
        if (toolbar != null) {
            toolbar.setVisibility(View.GONE);
        }
        getWindow().getDecorView().setSystemUiVisibility(
                View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY
                        | View.SYSTEM_UI_FLAG_FULLSCREEN
                        | View.SYSTEM_UI_FLAG_HIDE_NAVIGATION
                        | View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN
                        | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
                        | View.SYSTEM_UI_FLAG_LAYOUT_STABLE
        );
    }

    private void leaveReceiverFullscreen() {
        if (toolbar != null) {
            toolbar.setVisibility(View.VISIBLE);
        }
        getWindow().getDecorView().setSystemUiVisibility(View.SYSTEM_UI_FLAG_VISIBLE);
    }

    private void keepReceiverAwake(boolean enabled) {
        surfaceView.setKeepScreenOn(enabled);
        if (enabled) {
            getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        } else {
            getWindow().clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        }
    }

    private void setStatus(String text) {
        status.setText(text);
    }
}
