package com.screenmirror;

import android.Manifest;
import android.app.Activity;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.PackageManager;
import android.media.projection.MediaProjectionManager;
import android.net.wifi.WifiManager;
import android.os.Bundle;
import android.provider.Settings;
import android.text.InputFilter;
import android.text.InputType;
import android.util.DisplayMetrics;
import android.view.MotionEvent;
import android.view.SurfaceHolder;
import android.view.View;
import android.view.WindowManager;
import android.widget.Button;
import android.widget.CheckBox;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.TextView;

import java.util.ArrayList;
import java.util.List;

public final class MainActivity extends Activity implements ScreenCaptureService.Listener {
    private static final int STREAM_PORT = 5004;
    private static final String PREF_PIN = "pin";
    private static final String PREF_RECEIVE_AUDIO = "receive_audio";
    private static final String PREF_SEND_AUDIO = "send_audio";
    private static final String PREF_NOTIFICATION_PROMPTED = "notification_prompted";
    private static final int REQUEST_CAPTURE = 1001;
    private static final int REQUEST_RECORD_AUDIO = 1002;
    private static final int REQUEST_POST_NOTIFICATIONS = 1003;

    private final DiscoveryAgent discovery = new DiscoveryAgent();
    private final RtpH264Receiver receiver = new RtpH264Receiver();
    private final RtpOpusReceiver audioReceiver = new RtpOpusReceiver();
    private final ControlClient control = new ControlClient();
    private final ArrayList<DiscoveryAgent.Peer> selectedReceivers = new ArrayList<>();

    private MirrorSurfaceView surfaceView;
    private TextView status;
    private EditText pinInput;
    private CheckBox receiveAudio;
    private CheckBox sendAudio;
    private LinearLayout toolbar;
    private MediaProjectionManager projectionManager;
    private WifiManager.MulticastLock multicastLock;
    private SharedPreferences preferences;
    private boolean receiverRequested;
    private boolean receiverAudioEnabled;
    private String receiverPin = Pin.DEFAULT;

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        preferences = getSharedPreferences("screen-mirror", MODE_PRIVATE);
        projectionManager = (MediaProjectionManager) getSystemService(Context.MEDIA_PROJECTION_SERVICE);
        WifiManager wifi = (WifiManager) getApplicationContext().getSystemService(Context.WIFI_SERVICE);
        if (wifi != null) {
            multicastLock = wifi.createMulticastLock("screen-mirror-discovery");
            multicastLock.setReferenceCounted(false);
        }

        configureComponentListeners();
        surfaceView = new MirrorSurfaceView(this);
        surfaceView.getHolder().addCallback(new SurfaceHolder.Callback() {
            @Override
            public void surfaceCreated(SurfaceHolder holder) {
                startReceiverOnSurface(holder);
            }

            @Override
            public void surfaceChanged(SurfaceHolder holder, int format, int width, int height) {
            }

            @Override
            public void surfaceDestroyed(SurfaceHolder holder) {
                if (receiverRequested) {
                    receiver.stop();
                    audioReceiver.stop();
                }
            }
        });
        surfaceView.setOnTouchListener(this::sendTouchEvent);

        status = new TextView(this);
        status.setText(R.string.status_idle);
        status.setPadding(24, 24, 24, 24);

        pinInput = new EditText(this);
        pinInput.setHint(R.string.pin_hint);
        pinInput.setText(preferences.getString(PREF_PIN, Pin.DEFAULT));
        pinInput.setInputType(InputType.TYPE_CLASS_NUMBER | InputType.TYPE_NUMBER_VARIATION_PASSWORD);
        pinInput.setFilters(new InputFilter[]{new InputFilter.LengthFilter(4)});

        receiveAudio = new CheckBox(this);
        receiveAudio.setText(R.string.receive_audio);
        receiveAudio.setChecked(preferences.getBoolean(PREF_RECEIVE_AUDIO, false));

        sendAudio = new CheckBox(this);
        sendAudio.setText(R.string.send_audio);
        sendAudio.setChecked(preferences.getBoolean(PREF_SEND_AUDIO, false));

        Button startReceiver = button(R.string.start_receiver, view -> startReceiver());
        Button discover = button(R.string.discover_receivers, view -> discoverReceivers());
        Button startSender = button(R.string.start_sender, view -> startSender());
        Button accessibility = button(R.string.enable_touch_injection, view -> openAccessibilitySettings());
        Button diagnostics = button(R.string.copy_diagnostics, view -> copyDiagnostics());
        Button stop = button(R.string.stop, view -> stopAll());

        toolbar = new LinearLayout(this);
        toolbar.setOrientation(LinearLayout.VERTICAL);
        toolbar.addView(status);
        toolbar.addView(pinInput);
        toolbar.addView(receiveAudio);
        toolbar.addView(sendAudio);
        toolbar.addView(startReceiver);
        toolbar.addView(discover);
        toolbar.addView(startSender);
        toolbar.addView(accessibility);
        toolbar.addView(diagnostics);
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
        AppLog.info("main activity created");
    }

    @Override
    protected void onStart() {
        super.onStart();
        ScreenCaptureService.setListener(this);
    }

    @Override
    protected void onStop() {
        ScreenCaptureService.clearListener(this);
        super.onStop();
    }

    @Override
    protected void onDestroy() {
        receiverRequested = false;
        stopReceiverTransports();
        discovery.stop();
        releaseMulticast();
        control.close();
        AppLog.info("main activity destroyed; foreground sender left unchanged");
        super.onDestroy();
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode != REQUEST_CAPTURE) {
            return;
        }
        if (resultCode != RESULT_OK || data == null) {
            setStatus("Sender permission denied");
            return;
        }
        try {
            boolean audioEnabled = sendAudio.isChecked() && canSendAudio();
            ScreenCaptureService.start(
                    this,
                    resultCode,
                    data,
                    new ArrayList<>(selectedReceivers),
                    audioEnabled,
                    currentPinOrDefault()
            );
        } catch (Exception error) {
            AppLog.error("could not start foreground sender", error);
            setStatus("Sender failed: " + errorMessage(error));
        }
    }

    @Override
    public void onRequestPermissionsResult(int requestCode, String[] permissions, int[] grantResults) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults);
        if (requestCode != REQUEST_RECORD_AUDIO) {
            if (requestCode == REQUEST_POST_NOTIFICATIONS) {
                preferences.edit().putBoolean(PREF_NOTIFICATION_PROMPTED, true).apply();
                if (grantResults.length > 0 && grantResults[0] == PackageManager.PERMISSION_GRANTED) {
                    setStatus("Notification permission granted; continuing sender setup");
                } else {
                    setStatus("Notification permission denied; continuing with system foreground indicator");
                }
                startSender();
            }
            return;
        }
        if (grantResults.length > 0 && grantResults[0] == PackageManager.PERMISSION_GRANTED) {
            setStatus("Audio permission granted; tap Start Sender again");
        } else {
            setStatus("Audio permission denied; disable Send Audio or allow it in Settings");
        }
    }

    @Override
    public void onSenderStatus(String message, boolean active) {
        if (!receiverRequested) {
            setStatus(message);
        }
    }

    private void configureComponentListeners() {
        receiver.setListener(new RtpH264Receiver.Listener() {
            @Override
            public void onFirstPacket(String host) {
                runOnUiThread(() -> {
                    if (receiverRequested) {
                        setStatus(receiverStatus(receiverAudioEnabled) + " from " + host);
                    }
                });
            }

            @Override
            public void onDisconnected() {
                runOnUiThread(() -> {
                    if (!receiverRequested) {
                        return;
                    }
                    stopReceiverSession();
                    setStatus("Receiver disconnected: no video for 3 seconds");
                });
            }

            @Override
            public void onError(Throwable error) {
                runOnUiThread(() -> {
                    if (!receiverRequested) {
                        return;
                    }
                    stopReceiverSession();
                    setStatus("Receiver failed: " + errorMessage(error));
                });
            }
        });
        audioReceiver.setListener(error -> runOnUiThread(() -> {
            if (!receiverRequested || !receiverAudioEnabled) {
                return;
            }
            audioReceiver.stop();
            receiverAudioEnabled = false;
            setStatus(receiverStatus(false) + " (audio stopped: " + errorMessage(error) + ")");
        }));
        discovery.setListener(error -> runOnUiThread(() -> {
            if (!receiverRequested) {
                return;
            }
            stopReceiverSession();
            setStatus("Discovery beacon failed: " + errorMessage(error));
        }));
    }

    private boolean sendTouchEvent(View view, MotionEvent event) {
        String host = receiver.lastSenderHost();
        if (host == null || host.isEmpty()) {
            return true;
        }
        int maskedAction = event.getActionMasked();
        if (maskedAction == MotionEvent.ACTION_MOVE) {
            for (int index = 0; index < event.getPointerCount(); index++) {
                sendTouch(host, "move", view, event, index);
            }
            return true;
        }

        String action;
        switch (maskedAction) {
            case MotionEvent.ACTION_DOWN:
            case MotionEvent.ACTION_POINTER_DOWN:
                action = "down";
                break;
            case MotionEvent.ACTION_UP:
            case MotionEvent.ACTION_POINTER_UP:
                action = "up";
                view.performClick();
                break;
            case MotionEvent.ACTION_CANCEL:
                action = "cancel";
                break;
            default:
                return true;
        }
        sendTouch(host, action, view, event, event.getActionIndex());
        return true;
    }

    private void sendTouch(String host, String action, View view, MotionEvent event, int index) {
        float x = event.getX(index) / Math.max(1, view.getWidth());
        float y = event.getY(index) / Math.max(1, view.getHeight());
        control.send(host, action, x, y, event.getPointerId(index), currentPinOrDefault());
    }

    private Button button(int textResource, View.OnClickListener listener) {
        Button button = new Button(this);
        button.setText(textResource);
        button.setOnClickListener(listener);
        return button;
    }

    private void startReceiver() {
        String pin = currentPinOrStatus();
        if (pin == null) {
            return;
        }
        stopAll();
        receiverPin = pin;
        receiverAudioEnabled = receiveAudio.isChecked();
        receiverRequested = true;
        preferences.edit().putBoolean(PREF_RECEIVE_AUDIO, receiverAudioEnabled).apply();
        enterReceiverFullscreen();
        keepReceiverAwake(true);
        lockMulticast();
        setStatus("Status: waiting for video on :" + STREAM_PORT);
        SurfaceHolder holder = surfaceView.getHolder();
        if (holder.getSurface().isValid()) {
            startReceiverOnSurface(holder);
        }
    }

    private void startReceiverOnSurface(SurfaceHolder holder) {
        if (!receiverRequested || receiver.isRunning()) {
            return;
        }
        try {
            receiver.start(STREAM_PORT, holder.getSurface());
            if (receiverAudioEnabled) {
                audioReceiver.start(RtpOpusReceiver.DEFAULT_AUDIO_PORT);
            }
            discovery.startReceiverBeacon(
                    STREAM_PORT,
                    RtpOpusReceiver.DEFAULT_AUDIO_PORT,
                    displayWidth(),
                    displayHeight(),
                    displayRefreshHz(),
                    receiverPin
            );
            setStatus("Status: waiting for sender on :" + STREAM_PORT);
        } catch (Exception error) {
            AppLog.error("receiver startup failed", error);
            stopReceiverSession();
            setStatus("Receiver failed: " + errorMessage(error));
        }
    }

    private void discoverReceivers() {
        String pin = currentPinOrStatus();
        if (pin == null) {
            return;
        }
        lockMulticast();
        setStatus("Status: discovering receivers...");
        new Thread(() -> {
            try {
                List<DiscoveryAgent.Peer> peers = discovery.discoverReceivers(3000, pin);
                ArrayList<DiscoveryAgent.Peer> selected = selectReceivers(peers);
                runOnUiThread(() -> {
                    replaceSelectedReceivers(selected);
                    setStatus("Discovered " + selected.size() + " receiver(s): " + selected);
                    releaseMulticast();
                });
            } catch (Exception error) {
                AppLog.error("receiver discovery failed", error);
                runOnUiThread(() -> {
                    setStatus("Discovery failed: " + errorMessage(error));
                    releaseMulticast();
                });
            }
        }, "discover-receivers").start();
    }

    private void startSender() {
        String pin = currentPinOrStatus();
        if (pin == null) {
            return;
        }
        preferences.edit().putBoolean(PREF_SEND_AUDIO, sendAudio.isChecked()).apply();
        if (sendAudio.isChecked() && !ensureAudioPermission()) {
            return;
        }
        if (!ensureNotificationPermission()) {
            return;
        }
        receiverRequested = false;
        stopReceiverTransports();
        discovery.stop();
        leaveReceiverFullscreen();
        keepReceiverAwake(false);
        ScreenCaptureService.stop(this);
        lockMulticast();
        setStatus("Status: preparing sender...");
        ArrayList<DiscoveryAgent.Peer> existing = new ArrayList<>(selectedReceivers);
        new Thread(() -> {
            try {
                ArrayList<DiscoveryAgent.Peer> selected = existing;
                if (selected.isEmpty()) {
                    selected = selectReceivers(discovery.discoverReceivers(3000, pin));
                }
                ArrayList<DiscoveryAgent.Peer> finalSelected = selected;
                runOnUiThread(() -> {
                    replaceSelectedReceivers(finalSelected);
                    releaseMulticast();
                    if (finalSelected.isEmpty()) {
                        setStatus("No receivers found");
                        return;
                    }
                    startActivityForResult(
                            projectionManager.createScreenCaptureIntent(),
                            REQUEST_CAPTURE
                    );
                });
            } catch (Exception error) {
                AppLog.error("sender setup failed", error);
                runOnUiThread(() -> {
                    releaseMulticast();
                    setStatus("Sender setup failed: " + errorMessage(error));
                });
            }
        }, "sender-setup").start();
    }

    private void stopAll() {
        receiverRequested = false;
        stopReceiverTransports();
        discovery.stop();
        ScreenCaptureService.stop(this);
        leaveReceiverFullscreen();
        keepReceiverAwake(false);
        releaseMulticast();
        setStatus(getString(R.string.status_idle));
    }

    private void stopReceiverSession() {
        receiverRequested = false;
        stopReceiverTransports();
        discovery.stop();
        leaveReceiverFullscreen();
        keepReceiverAwake(false);
        releaseMulticast();
    }

    private void stopReceiverTransports() {
        receiver.stop();
        audioReceiver.stop();
    }

    private void replaceSelectedReceivers(List<DiscoveryAgent.Peer> peers) {
        selectedReceivers.clear();
        selectedReceivers.addAll(peers);
    }

    private static ArrayList<DiscoveryAgent.Peer> selectReceivers(List<DiscoveryAgent.Peer> peers) {
        ArrayList<DiscoveryAgent.Peer> selected = new ArrayList<>();
        for (int index = 0; index < peers.size() && index < 3; index++) {
            selected.add(peers.get(index));
        }
        return selected;
    }

    private void lockMulticast() {
        if (multicastLock != null && !multicastLock.isHeld()) {
            multicastLock.acquire();
        }
    }

    private void releaseMulticast() {
        if (multicastLock != null && multicastLock.isHeld()) {
            multicastLock.release();
        }
    }

    private void openAccessibilitySettings() {
        startActivity(new Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS));
    }

    private void copyDiagnostics() {
        String session = ScreenCaptureService.stateDescription()
                + "; receiver_requested=" + receiverRequested
                + "; video_receiver_running=" + receiver.isRunning()
                + "; audio_receiver_running=" + audioReceiver.isRunning();
        String report = AppLog.diagnostics(this, session);
        ClipboardManager clipboard = (ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
        clipboard.setPrimaryClip(ClipData.newPlainText("Screen Mirror diagnostics", report));
        setStatus("Diagnostics copied to clipboard");
    }

    private String currentPinOrStatus() {
        try {
            String pin = Pin.normalize(pinInput.getText().toString());
            preferences.edit().putString(PREF_PIN, pin).apply();
            return pin;
        } catch (IllegalArgumentException error) {
            setStatus("Invalid PIN: use exactly four digits");
            return null;
        }
    }

    private String currentPinOrDefault() {
        try {
            return Pin.normalize(pinInput.getText().toString());
        } catch (IllegalArgumentException error) {
            return Pin.DEFAULT;
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
        if (surfaceView == null) {
            return;
        }
        surfaceView.setKeepScreenOn(enabled);
        if (enabled) {
            getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        } else {
            getWindow().clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        }
    }

    private void setStatus(String text) {
        if (status != null) {
            status.setText(text);
        }
    }

    private boolean canSendAudio() {
        return android.os.Build.VERSION.SDK_INT >= 29
                && checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED;
    }

    private boolean ensureAudioPermission() {
        if (android.os.Build.VERSION.SDK_INT < 29) {
            setStatus("Audio sending requires Android 10 or newer");
            return false;
        }
        if (checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) {
            return true;
        }
        requestPermissions(new String[]{Manifest.permission.RECORD_AUDIO}, REQUEST_RECORD_AUDIO);
        setStatus("Audio permission requested; tap Start Sender again after allowing it");
        return false;
    }

    private boolean ensureNotificationPermission() {
        if (android.os.Build.VERSION.SDK_INT < 33
                || checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED
                || preferences.getBoolean(PREF_NOTIFICATION_PROMPTED, false)) {
            return true;
        }
        requestPermissions(
                new String[]{Manifest.permission.POST_NOTIFICATIONS},
                REQUEST_POST_NOTIFICATIONS
        );
        setStatus("Notification permission requested; tap Start Sender again after choosing");
        return false;
    }

    private static String receiverStatus(boolean audioEnabled) {
        if (audioEnabled) {
            return "Status: receiving video :" + STREAM_PORT + " and audio :" + RtpOpusReceiver.DEFAULT_AUDIO_PORT;
        }
        return "Status: receiving on :" + STREAM_PORT;
    }

    private static String errorMessage(Throwable error) {
        return error.getMessage() == null ? error.getClass().getSimpleName() : error.getMessage();
    }
}
