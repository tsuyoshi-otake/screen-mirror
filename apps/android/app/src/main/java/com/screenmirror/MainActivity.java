package com.screenmirror;

import android.Manifest;
import android.app.Activity;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.PackageManager;
import android.graphics.Typeface;
import android.media.AudioManager;
import android.media.projection.MediaProjectionManager;
import android.net.wifi.WifiManager;
import android.os.Bundle;
import android.provider.Settings;
import android.text.InputFilter;
import android.text.InputType;
import android.util.DisplayMetrics;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.MotionEvent;
import android.view.SurfaceHolder;
import android.view.View;
import android.view.WindowInsets;
import android.view.WindowManager;
import android.widget.CheckBox;
import android.widget.EditText;
import android.widget.FrameLayout;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.SeekBar;
import android.widget.TextView;

import java.util.ArrayList;
import java.util.List;

public final class MainActivity extends Activity implements ScreenCaptureService.Listener {
    private static final int STREAM_PORT = 5004;
    private static final String PREF_PIN = "pin";
    private static final String PREF_RECEIVE_AUDIO = "receive_audio";
    private static final String PREF_SEND_AUDIO = "send_audio";
    private static final String PREF_NOTIFICATION_PROMPTED = "notification_prompted";
    private static final String PREF_RECEIVE_VOLUME = "receive_volume_percent";
    private static final int DEFAULT_VOLUME_PERCENT = 100;
    private static final int MAX_VOLUME_PERCENT = 400;
    private static final int REQUEST_CAPTURE = 1001;
    private static final int REQUEST_RECORD_AUDIO = 1002;
    private static final int REQUEST_POST_NOTIFICATIONS = 1003;

    private final DiscoveryAgent discovery = new DiscoveryAgent();
    private final RtpH264Receiver receiver = new RtpH264Receiver();
    private final RtpOpusReceiver audioReceiver = new RtpOpusReceiver();
    private final ControlClient control = new ControlClient();
    private final ArrayList<DiscoveryAgent.Peer> selectedReceivers = new ArrayList<>();

    private MirrorSurfaceView surfaceView;
    private FrameLayout mirrorStage;
    private TextView status;
    private EditText pinInput;
    private CheckBox receiveAudio;
    private CheckBox sendAudio;
    private TextView volumeLabel;
    private SeekBar volumeBar;
    private LinearLayout toolbar;
    private ScrollView controls;
    private MediaProjectionManager projectionManager;
    private WifiManager.MulticastLock multicastLock;
    private SharedPreferences preferences;
    private boolean receiverRequested;
    private boolean receiverAudioEnabled;
    private boolean senderActive;
    private boolean updatingAudioControls;
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
                AppLog.info("mirror surface created (receiver requested: " + receiverRequested + ")");
                startReceiverOnSurface(holder);
            }

            @Override
            public void surfaceChanged(SurfaceHolder holder, int format, int width, int height) {
            }

            @Override
            public void surfaceDestroyed(SurfaceHolder holder) {
                if (receiverRequested) {
                    AppLog.info("mirror surface destroyed; pausing the receiver until it comes back");
                    receiver.stop();
                    audioReceiver.stop();
                }
            }
        });
        surfaceView.setOnTouchListener(this::sendTouchEvent);

        TextView appTitle = new TextView(this);
        appTitle.setText(R.string.app_name);
        appTitle.setTextColor(Ui.TEXT);
        appTitle.setTypeface(Typeface.DEFAULT_BOLD);
        appTitle.setTextSize(TypedValue.COMPLEX_UNIT_SP, 22);

        status = Ui.statusPill(this);
        status.setText(R.string.status_idle);

        pinInput = new EditText(this);
        pinInput.setHint(R.string.pin_hint);
        pinInput.setText(preferences.getString(PREF_PIN, Pin.DEFAULT));
        pinInput.setInputType(InputType.TYPE_CLASS_NUMBER | InputType.TYPE_NUMBER_VARIATION_PASSWORD);
        pinInput.setFilters(new InputFilter[]{new InputFilter.LengthFilter(4)});
        pinInput.setTextColor(Ui.TEXT);
        pinInput.setHintTextColor(Ui.TEXT_MUTED);
        pinInput.setBackground(Ui.fieldBackground(this));
        pinInput.setPadding(Ui.dp(this, 14), Ui.dp(this, 12), Ui.dp(this, 14), Ui.dp(this, 12));
        pinInput.setTextSize(TypedValue.COMPLEX_UNIT_SP, 20);
        pinInput.setLetterSpacing(0.5f);
        pinInput.setGravity(Gravity.CENTER);

        receiveAudio = new CheckBox(this);
        receiveAudio.setText(R.string.receive_audio);
        receiveAudio.setChecked(preferences.getBoolean(PREF_RECEIVE_AUDIO, false));
        receiveAudio.setOnCheckedChangeListener(
                (button, enabled) -> onReceiveAudioChanged(enabled)
        );
        Ui.tint(receiveAudio);

        volumeLabel = Ui.label(this, "");
        volumeBar = new SeekBar(this);
        Ui.tint(volumeBar);
        volumeBar.setMax(MAX_VOLUME_PERCENT);
        volumeBar.setProgress(preferences.getInt(PREF_RECEIVE_VOLUME, DEFAULT_VOLUME_PERCENT));
        volumeBar.setOnSeekBarChangeListener(new SeekBar.OnSeekBarChangeListener() {
            @Override
            public void onProgressChanged(SeekBar bar, int percent, boolean fromUser) {
                applyReceiveVolume(percent, fromUser);
            }

            @Override
            public void onStartTrackingTouch(SeekBar bar) {
            }

            @Override
            public void onStopTrackingTouch(SeekBar bar) {
            }
        });
        applyReceiveVolume(volumeBar.getProgress(), false);

        sendAudio = new CheckBox(this);
        sendAudio.setText(R.string.send_audio);
        sendAudio.setChecked(preferences.getBoolean(PREF_SEND_AUDIO, false));
        sendAudio.setOnCheckedChangeListener(
                (button, enabled) -> onSendAudioChanged(enabled)
        );
        Ui.tint(sendAudio);

        LinearLayout header = new LinearLayout(this);
        header.setOrientation(LinearLayout.VERTICAL);
        header.setPadding(Ui.dp(this, 20), Ui.dp(this, 20), Ui.dp(this, 20), 0);
        header.addView(appTitle);
        header.addView(status, stacked(10, false));

        LinearLayout pinCard = Ui.card(this);
        pinCard.addView(Ui.sectionTitle(this, getString(R.string.pin_label)));
        pinCard.addView(pinInput, stacked(12, true));

        LinearLayout receiveCard = Ui.card(this);
        receiveCard.addView(Ui.sectionTitle(this, getString(R.string.section_receive)));
        receiveCard.addView(Ui.caption(this, getString(R.string.section_receive_hint)));
        receiveCard.addView(receiveAudio);
        receiveCard.addView(volumeLabel, stacked(10, true));
        receiveCard.addView(volumeBar, stacked(2, true));
        receiveCard.addView(Ui.caption(this, getString(R.string.receive_volume_hint)));
        receiveCard.addView(
                Ui.primaryButton(this, getString(R.string.start_receiver), view -> startReceiver()),
                stacked(6, true)
        );

        LinearLayout senderRow = Ui.row(this);
        senderRow.addView(
                Ui.secondaryButton(this, getString(R.string.discover_receivers), view -> discoverReceivers()),
                Ui.weighted(this)
        );
        senderRow.addView(
                Ui.primaryButton(this, getString(R.string.start_sender), view -> startSender()),
                Ui.weighted(this)
        );

        LinearLayout sendCard = Ui.card(this);
        sendCard.addView(Ui.sectionTitle(this, getString(R.string.section_send)));
        sendCard.addView(Ui.caption(this, getString(R.string.section_send_hint)));
        sendCard.addView(sendAudio);
        sendCard.addView(senderRow, stacked(12, true));

        LinearLayout advancedRow = Ui.row(this);
        advancedRow.addView(
                Ui.quietButton(this, getString(R.string.enable_touch_injection), view -> openAccessibilitySettings()),
                Ui.weighted(this)
        );
        advancedRow.addView(
                Ui.quietButton(this, getString(R.string.copy_diagnostics), view -> copyDiagnostics()),
                Ui.weighted(this)
        );

        LinearLayout advancedCard = Ui.card(this);
        advancedCard.addView(Ui.sectionTitle(this, getString(R.string.section_advanced)));
        advancedCard.addView(Ui.caption(this, getString(R.string.section_advanced_hint)));
        advancedCard.addView(advancedRow);

        toolbar = new LinearLayout(this);
        toolbar.setOrientation(LinearLayout.VERTICAL);
        toolbar.addView(header);
        toolbar.addView(pinCard, Ui.cardParams(this));
        toolbar.addView(receiveCard, Ui.cardParams(this));
        toolbar.addView(sendCard, Ui.cardParams(this));
        toolbar.addView(advancedCard, Ui.cardParams(this));
        toolbar.addView(
                Ui.dangerButton(this, getString(R.string.stop), view -> stopAll()),
                Ui.cardParams(this)
        );

        controls = new ScrollView(this);
        controls.addView(toolbar);
        controls.setClipToPadding(true);
        // targetSdk 35 draws edge to edge, so the controls keep clear of the status and navigation bars.
        controls.setOnApplyWindowInsetsListener((view, insets) -> {
            if (android.os.Build.VERSION.SDK_INT >= 30) {
                android.graphics.Insets bars = insets.getInsets(WindowInsets.Type.systemBars());
                view.setPadding(bars.left, bars.top, bars.right, bars.bottom);
            } else {
                view.setPadding(
                        insets.getSystemWindowInsetLeft(),
                        insets.getSystemWindowInsetTop(),
                        insets.getSystemWindowInsetRight(),
                        insets.getSystemWindowInsetBottom()
                );
            }
            return insets;
        });

        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        root.setBackgroundColor(Ui.BACKGROUND);
        root.addView(controls, new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT,
                0,
                1.0f
        ));
        // The surface keeps the sender's aspect ratio, so it needs a black stage to be centred in.
        mirrorStage = new FrameLayout(this);
        mirrorStage.setBackgroundColor(0xFF000000);
        mirrorStage.setVisibility(View.GONE);
        mirrorStage.addView(surfaceView, new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.WRAP_CONTENT,
                FrameLayout.LayoutParams.WRAP_CONTENT,
                Gravity.CENTER
        ));
        root.addView(mirrorStage, new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT,
                0,
                1.0f
        ));
        setContentView(root);
        // Hardware volume keys stay useful while the receiver runs fullscreen with the toolbar hidden.
        setVolumeControlStream(AudioManager.STREAM_MUSIC);
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
            setStatus("画面キャプチャが許可されませんでした");
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
            setStatus("送信エラー: " + errorMessage(error));
        }
    }

    @Override
    public void onRequestPermissionsResult(int requestCode, String[] permissions, int[] grantResults) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults);
        if (requestCode != REQUEST_RECORD_AUDIO) {
            if (requestCode == REQUEST_POST_NOTIFICATIONS) {
                preferences.edit().putBoolean(PREF_NOTIFICATION_PROMPTED, true).apply();
                if (grantResults.length > 0 && grantResults[0] == PackageManager.PERMISSION_GRANTED) {
                    setStatus("通知が許可されました。送信の準備を続けます");
                } else {
                    setStatus("通知は許可されませんでした。システム表示のまま続行します");
                }
                startSender();
            }
            return;
        }
        if (grantResults.length > 0 && grantResults[0] == PackageManager.PERMISSION_GRANTED) {
            setStatus("マイク権限が許可されました。「音声も送る」を有効にするか送信を開始してください");
        } else {
            setStatus("マイク権限が拒否されました。「音声も送る」を外すか設定で許可してください");
        }
    }

    @Override
    public void onSenderStatus(String message, boolean active) {
        senderActive = active;
        if (active) {
            setSendAudioChecked(ScreenCaptureService.isAudioEnabled());
        }
        if (!receiverRequested) {
            setStatus(message);
        }
    }

    private void onReceiveAudioChanged(boolean enabled) {
        if (updatingAudioControls) {
            return;
        }
        preferences.edit().putBoolean(PREF_RECEIVE_AUDIO, enabled).apply();
        if (!receiverRequested) {
            return;
        }

        receiverAudioEnabled = enabled;
        try {
            if (enabled && receiver.isRunning()) {
                audioReceiver.start(RtpOpusReceiver.DEFAULT_AUDIO_PORT);
            } else if (!enabled) {
                audioReceiver.stop();
            }
            setStatus(receiverStatus(enabled));
            AppLog.info("receiver audio "
                    + (enabled ? "enabled" : "disabled")
                    + " without restarting video");
        } catch (Exception error) {
            receiverAudioEnabled = false;
            audioReceiver.stop();
            setReceiveAudioChecked(false);
            preferences.edit().putBoolean(PREF_RECEIVE_AUDIO, false).apply();
            AppLog.error("receiver audio toggle failed; video remains active", error);
            setStatus(receiverStatus(false) + "（音声の切り替えに失敗: "
                    + errorMessage(error) + ")");
        }
    }

    private void onSendAudioChanged(boolean enabled) {
        if (updatingAudioControls) {
            return;
        }
        preferences.edit().putBoolean(PREF_SEND_AUDIO, enabled).apply();
        if (!senderActive) {
            return;
        }
        if (enabled && !canSendAudio()) {
            setSendAudioChecked(false);
            preferences.edit().putBoolean(PREF_SEND_AUDIO, false).apply();
            ensureAudioPermission();
            return;
        }

        try {
            ScreenCaptureService.setAudioEnabled(this, enabled);
            AppLog.info("requested sender audio "
                    + (enabled ? "enable" : "disable")
                    + " without restarting video");
        } catch (RuntimeException error) {
            setSendAudioChecked(ScreenCaptureService.isAudioEnabled());
            AppLog.error("sender audio toggle failed; video remains active", error);
            setStatus("送信音声の切り替えに失敗: " + errorMessage(error));
        }
    }

    private void setReceiveAudioChecked(boolean enabled) {
        updatingAudioControls = true;
        receiveAudio.setChecked(enabled);
        updatingAudioControls = false;
    }

    private void setSendAudioChecked(boolean enabled) {
        updatingAudioControls = true;
        sendAudio.setChecked(enabled);
        updatingAudioControls = false;
    }

    private void configureComponentListeners() {
        receiver.setListener(new RtpH264Receiver.Listener() {
            @Override
            public void onFirstPacket(String host) {
                runOnUiThread(() -> {
                    if (receiverRequested) {
                        setStatus(receiverStatus(receiverAudioEnabled) + "（" + host + "）");
                    }
                });
            }

            @Override
            public void onVideoSize(int width, int height) {
                runOnUiThread(() -> surfaceView.setVideoSize(width, height));
            }

            @Override
            public void onDisconnected() {
                runOnUiThread(() -> {
                    if (!receiverRequested) {
                        return;
                    }
                    stopReceiverSession();
                    setStatus("受信が切断されました（3秒間映像なし）");
                });
            }

            @Override
            public void onError(Throwable error) {
                runOnUiThread(() -> {
                    if (!receiverRequested) {
                        return;
                    }
                    stopReceiverSession();
                    setStatus("受信エラー: " + errorMessage(error));
                });
            }
        });
        audioReceiver.setListener(error -> runOnUiThread(() -> {
            if (!receiverRequested || !receiverAudioEnabled) {
                return;
            }
            audioReceiver.stop();
            receiverAudioEnabled = false;
            setReceiveAudioChecked(false);
            preferences.edit().putBoolean(PREF_RECEIVE_AUDIO, false).apply();
            setStatus(receiverStatus(false) + "（音声を停止: " + errorMessage(error) + "）");
        }));
        discovery.setListener(error -> runOnUiThread(() -> {
            if (!receiverRequested) {
                return;
            }
            stopReceiverSession();
            setStatus("受信側の告知に失敗: " + errorMessage(error));
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

    private LinearLayout.LayoutParams stacked(int topMarginDp, boolean matchWidth) {
        LinearLayout.LayoutParams params = new LinearLayout.LayoutParams(
                matchWidth ? LinearLayout.LayoutParams.MATCH_PARENT : LinearLayout.LayoutParams.WRAP_CONTENT,
                LinearLayout.LayoutParams.WRAP_CONTENT
        );
        params.topMargin = Ui.dp(this, topMarginDp);
        return params;
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
        mirrorStage.setVisibility(View.VISIBLE);
        enterReceiverFullscreen();
        keepReceiverAwake(true);
        lockMulticast();
        setStatus("受信待機中 :" + STREAM_PORT);
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
            setStatus("送信側の接続を待っています :" + STREAM_PORT);
        } catch (Exception error) {
            AppLog.error("receiver startup failed", error);
            stopReceiverSession();
            setStatus("受信エラー: " + errorMessage(error));
        }
    }

    private void discoverReceivers() {
        String pin = currentPinOrStatus();
        if (pin == null) {
            return;
        }
        lockMulticast();
        setStatus("受信側を検索中…");
        new Thread(() -> {
            try {
                List<DiscoveryAgent.Peer> peers = discovery.discoverReceivers(3000, pin);
                ArrayList<DiscoveryAgent.Peer> selected = selectReceivers(peers);
                runOnUiThread(() -> {
                    replaceSelectedReceivers(selected);
                    setStatus("受信側を " + selected.size() + " 台検出: " + selected);
                    releaseMulticast();
                });
            } catch (Exception error) {
                AppLog.error("receiver discovery failed", error);
                runOnUiThread(() -> {
                    setStatus("検索に失敗: " + errorMessage(error));
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
        setStatus("送信を準備中…");
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
                        setStatus("受信側が見つかりません");
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
                    setStatus("送信の準備に失敗: " + errorMessage(error));
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
        setStatus("診断情報をコピーしました");
    }

    private String currentPinOrStatus() {
        try {
            String pin = Pin.normalize(pinInput.getText().toString());
            preferences.edit().putString(PREF_PIN, pin).apply();
            return pin;
        } catch (IllegalArgumentException error) {
            setStatus("PIN は4桁の数字で入力してください");
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
        if (controls != null) {
            controls.setVisibility(View.GONE);
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
        if (controls != null) {
            controls.setVisibility(View.VISIBLE);
        }
        if (mirrorStage != null && !receiverRequested) {
            mirrorStage.setVisibility(View.GONE);
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

    private void applyReceiveVolume(int percent, boolean persist) {
        if (volumeLabel != null) {
            volumeLabel.setText(getString(R.string.receive_volume, percent));
        }
        audioReceiver.setGain(percent / 100.0f);
        if (persist) {
            preferences.edit().putInt(PREF_RECEIVE_VOLUME, percent).apply();
        }
    }

    @Override
    public void onBackPressed() {
        // Back leaves the fullscreen receiver first so the controls (volume, stop) come back
        // without tearing down the session; a second press stops it.
        if (receiverRequested && controls != null && controls.getVisibility() != View.VISIBLE) {
            leaveReceiverFullscreen();
            setStatus(receiverStatus(receiverAudioEnabled) + "（もう一度戻るで停止）");
            return;
        }
        if (receiverRequested) {
            stopAll();
            return;
        }
        super.onBackPressed();
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
            setStatus("音声送信は Android 10 以上が必要です");
            return false;
        }
        if (checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) {
            return true;
        }
        requestPermissions(new String[]{Manifest.permission.RECORD_AUDIO}, REQUEST_RECORD_AUDIO);
        setStatus("マイク権限を要求しました。許可後にもう一度「音声も送る」を有効にしてください");
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
        setStatus("通知権限を要求しました。選択後にもう一度「送信を開始」を押してください");
        return false;
    }

    private static String receiverStatus(boolean audioEnabled) {
        if (audioEnabled) {
            return "受信中 映像:" + STREAM_PORT + " 音声:" + RtpOpusReceiver.DEFAULT_AUDIO_PORT;
        }
        return "受信中 映像:" + STREAM_PORT;
    }

    private static String errorMessage(Throwable error) {
        return error.getMessage() == null ? error.getClass().getSimpleName() : error.getMessage();
    }
}
