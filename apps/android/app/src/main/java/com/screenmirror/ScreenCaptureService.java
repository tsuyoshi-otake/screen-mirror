package com.screenmirror;

import android.Manifest;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.content.pm.ServiceInfo;
import android.media.projection.MediaProjection;
import android.media.projection.MediaProjectionManager;
import android.os.Build;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;

import org.json.JSONArray;
import org.json.JSONObject;

import java.lang.ref.WeakReference;
import java.util.ArrayList;
import java.util.List;

public final class ScreenCaptureService extends Service {
    interface Listener {
        void onSenderStatus(String message, boolean active);
    }

    private static final String ACTION_START = "com.screenmirror.action.START_CAPTURE";
    private static final String ACTION_STOP = "com.screenmirror.action.STOP_CAPTURE";
    private static final String ACTION_SET_AUDIO = "com.screenmirror.action.SET_AUDIO";
    private static final String EXTRA_RESULT_CODE = "result_code";
    private static final String EXTRA_CAPTURE_DATA = "capture_data";
    private static final String EXTRA_PEERS = "peers";
    private static final String EXTRA_AUDIO = "audio";
    private static final String EXTRA_PIN = "pin";
    private static final String CHANNEL_ID = "screen_capture";
    private static final int NOTIFICATION_ID = 1001;

    private static final Object STATE_LOCK = new Object();
    private static WeakReference<Listener> statusListener = new WeakReference<>(null);
    private static boolean active;
    private static boolean currentAudioEnabled;
    private static String currentStatus = "Status: idle";

    private final Handler mainHandler = new Handler(Looper.getMainLooper());
    private ScreenSender sender;
    private TouchControlServer touchServer;
    private boolean foregroundStarted;
    private boolean shuttingDown;
    private boolean audioEnabled;
    private boolean touchEnabled;
    private int peerCount;

    static void start(
            Context context,
            int resultCode,
            Intent captureData,
            List<DiscoveryAgent.Peer> peers,
            boolean audioEnabled,
            String pin
    ) throws Exception {
        Intent intent = new Intent(context, ScreenCaptureService.class);
        intent.setAction(ACTION_START);
        intent.putExtra(EXTRA_RESULT_CODE, resultCode);
        intent.putExtra(EXTRA_CAPTURE_DATA, captureData);
        intent.putExtra(EXTRA_PEERS, encodePeers(peers));
        intent.putExtra(EXTRA_AUDIO, audioEnabled);
        intent.putExtra(EXTRA_PIN, Pin.normalize(pin));
        publishStatus("Status: starting sender...", false);
        try {
            context.startForegroundService(intent);
        } catch (RuntimeException error) {
            publishStatus("Sender failed: " + error.getClass().getSimpleName(), false);
            throw error;
        }
    }

    static void stop(Context context) {
        boolean stopped = context.stopService(new Intent(context, ScreenCaptureService.class));
        if (!stopped) {
            setCurrentAudioEnabled(false);
            publishStatus("Status: idle", false);
        }
    }

    static void setAudioEnabled(Context context, boolean enabled) {
        Intent intent = new Intent(context, ScreenCaptureService.class);
        intent.setAction(ACTION_SET_AUDIO);
        intent.putExtra(EXTRA_AUDIO, enabled);
        context.startService(intent);
    }

    static void setListener(Listener listener) {
        String status;
        boolean isActive;
        synchronized (STATE_LOCK) {
            statusListener = new WeakReference<>(listener);
            status = currentStatus;
            isActive = active;
        }
        listener.onSenderStatus(status, isActive);
    }

    static void clearListener(Listener listener) {
        synchronized (STATE_LOCK) {
            if (statusListener.get() == listener) {
                statusListener.clear();
            }
        }
    }

    static String stateDescription() {
        synchronized (STATE_LOCK) {
            return currentStatus + "; active=" + active + "; audio=" + currentAudioEnabled;
        }
    }

    static boolean isAudioEnabled() {
        synchronized (STATE_LOCK) {
            return currentAudioEnabled;
        }
    }

    @Override
    public void onCreate() {
        super.onCreate();
        createNotificationChannel();
        sender = new ScreenSender(this, new ScreenSender.Listener() {
            @Override
            public void onError(Throwable error) {
                mainHandler.post(() -> failSession("Sender failed", error));
            }

            @Override
            public void onAudioError(Throwable error) {
                mainHandler.post(() -> handleAudioFailure(error));
            }

            @Override
            public void onProjectionStopped() {
                mainHandler.post(() -> finishSession("Sender stopped: screen capture permission ended"));
            }
        });
        touchServer = new TouchControlServer();
        AppLog.info("screen capture service created");
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        if (intent == null) {
            stopSelf(startId);
            return START_NOT_STICKY;
        }
        if (ACTION_STOP.equals(intent.getAction())) {
            finishSession("Status: idle");
            return START_NOT_STICKY;
        }
        if (ACTION_SET_AUDIO.equals(intent.getAction())) {
            try {
                changeAudioTransfer(intent.getBooleanExtra(EXTRA_AUDIO, false));
            } catch (Exception error) {
                AppLog.error("sender audio toggle failed; video remains active", error);
                String detail = error.getMessage() == null
                        ? error.getClass().getSimpleName()
                        : error.getMessage();
                boolean videoActive = sender != null && sender.isRunning();
                String status = videoActive
                        ? sessionStatus() + " (audio change failed: " + detail + ")"
                        : "Status: idle (audio change ignored: " + detail + ")";
                publishStatus(status, videoActive);
                if (videoActive) {
                    updateNotification(status);
                }
            }
            return START_NOT_STICKY;
        }
        if (!ACTION_START.equals(intent.getAction())) {
            stopSelf(startId);
            return START_NOT_STICKY;
        }

        try {
            startInForeground(getString(R.string.notification_starting));
            startSession(intent);
        } catch (Exception error) {
            failSession("Sender failed", error);
        }
        return START_NOT_STICKY;
    }

    @Override
    public void onDestroy() {
        shuttingDown = true;
        cleanupSession();
        if (foregroundStarted) {
            stopForeground(STOP_FOREGROUND_REMOVE);
            foregroundStarted = false;
        }
        synchronized (STATE_LOCK) {
            if (active) {
                publishStatus("Status: idle", false);
            }
        }
        AppLog.info("screen capture service destroyed");
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    private void startSession(Intent intent) throws Exception {
        cleanupSession();
        shuttingDown = false;

        boolean audioEnabled = intent.getBooleanExtra(EXTRA_AUDIO, false);
        if (audioEnabled && checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
            throw new SecurityException("record audio permission is not granted");
        }

        Intent captureData = captureData(intent);
        if (captureData == null) {
            throw new IllegalArgumentException("screen capture permission data is missing");
        }
        List<DiscoveryAgent.Peer> peers = decodePeers(intent.getStringExtra(EXTRA_PEERS));
        if (peers.isEmpty()) {
            throw new IllegalArgumentException("no receiver targets were supplied");
        }
        String pin = Pin.normalize(intent.getStringExtra(EXTRA_PIN));

        MediaProjectionManager manager =
                (MediaProjectionManager) getSystemService(Context.MEDIA_PROJECTION_SERVICE);
        MediaProjection projection = manager.getMediaProjection(
                intent.getIntExtra(EXTRA_RESULT_CODE, 0),
                captureData
        );
        if (projection == null) {
            throw new IllegalStateException("failed to obtain screen capture permission");
        }

        sender.start(projection, peers, audioEnabled);
        this.audioEnabled = audioEnabled;
        setCurrentAudioEnabled(audioEnabled);
        peerCount = peers.size();
        touchEnabled = false;
        try {
            touchServer.start(pin);
            touchEnabled = touchServer.isInjectingEnabled();
        } catch (Exception error) {
            AppLog.warn("touch control could not start; video will continue", error);
        }

        String message = sessionStatus();
        publishStatus(message, true);
        updateNotification(message);
    }

    private void changeAudioTransfer(boolean enabled) throws Exception {
        if (sender == null || !sender.isRunning()) {
            throw new IllegalStateException("screen sender is not running");
        }
        if (enabled
                && checkSelfPermission(Manifest.permission.RECORD_AUDIO)
                != PackageManager.PERMISSION_GRANTED) {
            throw new SecurityException("record audio permission is not granted");
        }
        sender.setAudioEnabled(enabled);
        audioEnabled = enabled;
        setCurrentAudioEnabled(enabled);
        String message = sessionStatus();
        publishStatus(message, true);
        updateNotification(message);
    }

    private void handleAudioFailure(Throwable error) {
        if (sender == null || !sender.isRunning()) {
            return;
        }
        try {
            sender.setAudioEnabled(false);
        } catch (Exception cleanupError) {
            AppLog.warn("sender audio cleanup failed; video remains active", cleanupError);
        }
        audioEnabled = false;
        setCurrentAudioEnabled(false);
        String detail = error.getMessage() == null
                ? error.getClass().getSimpleName()
                : error.getMessage();
        String message = sessionStatus() + " (audio stopped: " + detail + ")";
        publishStatus(message, true);
        updateNotification(message);
    }

    private String sessionStatus() {
        String profile = sender == null ? "screen" : sender.profileDescription();
        return "Status: sending " + profile
                + " to " + peerCount + " receiver(s)"
                + (audioEnabled ? " with audio" : "")
                + (touchEnabled ? " with touch" : " (enable Accessibility for touch)");
    }

    private void failSession(String prefix, Throwable error) {
        if (shuttingDown) {
            return;
        }
        String detail = error.getMessage() == null ? error.getClass().getSimpleName() : error.getMessage();
        AppLog.error(prefix, error);
        finishSession(prefix + ": " + detail);
    }

    private void finishSession(String status) {
        if (shuttingDown) {
            return;
        }
        shuttingDown = true;
        cleanupSession();
        publishStatus(status, false);
        if (foregroundStarted) {
            stopForeground(STOP_FOREGROUND_REMOVE);
            foregroundStarted = false;
        }
        stopSelf();
    }

    private void cleanupSession() {
        if (touchServer != null) {
            touchServer.stop();
        }
        if (sender != null) {
            sender.stop();
        }
        audioEnabled = false;
        setCurrentAudioEnabled(false);
        touchEnabled = false;
        peerCount = 0;
    }

    private void startInForeground(String text) {
        Notification notification = notification(text);
        if (Build.VERSION.SDK_INT >= 29) {
            startForeground(
                    NOTIFICATION_ID,
                    notification,
                    ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION
            );
        } else {
            startForeground(NOTIFICATION_ID, notification);
        }
        foregroundStarted = true;
    }

    private void updateNotification(String text) {
        if (Build.VERSION.SDK_INT >= 33
                && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
            AppLog.warn("notification permission is denied; foreground capture remains active", null);
            return;
        }
        NotificationManager manager = getSystemService(NotificationManager.class);
        manager.notify(NOTIFICATION_ID, notification(text));
    }

    private Notification notification(String text) {
        Intent openIntent = new Intent(this, MainActivity.class);
        PendingIntent contentIntent = PendingIntent.getActivity(
                this,
                0,
                openIntent,
                PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE
        );
        Intent stopIntent = new Intent(this, ScreenCaptureService.class).setAction(ACTION_STOP);
        PendingIntent stopPendingIntent = PendingIntent.getService(
                this,
                1,
                stopIntent,
                PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE
        );
        return new Notification.Builder(this, CHANNEL_ID)
                .setSmallIcon(R.drawable.ic_screen_mirror)
                .setContentTitle(getString(R.string.notification_title))
                .setContentText(text)
                .setContentIntent(contentIntent)
                .setOngoing(true)
                .setCategory(Notification.CATEGORY_SERVICE)
                .addAction(new Notification.Action.Builder(
                        null,
                        getString(R.string.stop),
                        stopPendingIntent
                ).build())
                .build();
    }

    private void createNotificationChannel() {
        NotificationChannel channel = new NotificationChannel(
                CHANNEL_ID,
                getString(R.string.notification_channel_name),
                NotificationManager.IMPORTANCE_LOW
        );
        channel.setDescription(getString(R.string.notification_channel_description));
        getSystemService(NotificationManager.class).createNotificationChannel(channel);
    }

    @SuppressWarnings("deprecation")
    private static Intent captureData(Intent intent) {
        if (Build.VERSION.SDK_INT >= 33) {
            return intent.getParcelableExtra(EXTRA_CAPTURE_DATA, Intent.class);
        }
        return intent.getParcelableExtra(EXTRA_CAPTURE_DATA);
    }

    private static String encodePeers(List<DiscoveryAgent.Peer> peers) throws Exception {
        JSONArray array = new JSONArray();
        for (DiscoveryAgent.Peer peer : peers) {
            JSONObject json = new JSONObject();
            json.put("instance_id", peer.instanceId);
            json.put("device_name", peer.deviceName);
            json.put("role", peer.role);
            json.put("host", peer.host);
            json.put("stream_port", peer.streamPort);
            json.put("audio_port", peer.audioPort);
            json.put("display_width", peer.displayWidth);
            json.put("display_height", peer.displayHeight);
            json.put("refresh_hz", peer.refreshHz);
            array.put(json);
        }
        return array.toString();
    }

    private static List<DiscoveryAgent.Peer> decodePeers(String text) throws Exception {
        ArrayList<DiscoveryAgent.Peer> peers = new ArrayList<>();
        JSONArray array = new JSONArray(text == null ? "[]" : text);
        for (int index = 0; index < array.length(); index++) {
            JSONObject json = array.getJSONObject(index);
            peers.add(new DiscoveryAgent.Peer(
                    json.getString("instance_id"),
                    json.getString("device_name"),
                    json.getString("role"),
                    json.getString("host"),
                    json.getInt("stream_port"),
                    json.getInt("audio_port"),
                    json.optInt("display_width", 0),
                    json.optInt("display_height", 0),
                    json.optInt("refresh_hz", 0)
            ));
        }
        return peers;
    }

    private static void publishStatus(String message, boolean isActive) {
        Listener listener;
        synchronized (STATE_LOCK) {
            currentStatus = message;
            active = isActive;
            listener = statusListener.get();
        }
        if (listener != null) {
            listener.onSenderStatus(message, isActive);
        }
    }

    private static void setCurrentAudioEnabled(boolean enabled) {
        synchronized (STATE_LOCK) {
            currentAudioEnabled = enabled;
        }
    }
}
