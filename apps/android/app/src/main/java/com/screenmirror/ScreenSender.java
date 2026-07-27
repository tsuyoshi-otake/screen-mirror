package com.screenmirror;

import android.content.Context;
import android.hardware.display.DisplayManager;
import android.hardware.display.VirtualDisplay;
import android.media.MediaCodec;
import android.media.MediaCodecInfo;
import android.media.MediaFormat;
import android.media.projection.MediaProjection;
import android.os.Handler;
import android.os.Looper;
import android.util.Range;
import android.view.Surface;

import java.nio.ByteBuffer;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.atomic.AtomicBoolean;

final class ScreenSender {
    interface Listener {
        void onError(Throwable error);

        void onProjectionStopped();
    }

    private static final int DEFAULT_WIDTH = 1280;
    private static final int DEFAULT_HEIGHT = 720;
    private static final int DPI = 320;
    private static final int DEFAULT_FPS = 30;
    private static final int MIN_FPS = 24;
    private static final int MAX_FPS = 30;
    private static final int MIN_BITRATE = 3_000_000;
    private static final int MAX_BITRATE = 10_000_000;
    private static final int MAX_WIDTH = 1920;
    private static final int MAX_HEIGHT = 1080;

    private final Listener listener;
    private final AtomicBoolean running = new AtomicBoolean(false);
    private final AtomicBoolean failureReported = new AtomicBoolean(false);
    private final RtpPacketizer packetizer = new RtpPacketizer();
    private final AudioSender audioSender;
    private final Handler mainHandler = new Handler(Looper.getMainLooper());

    private Thread thread;
    private MediaCodec encoder;
    private Surface inputSurface;
    private VirtualDisplay virtualDisplay;
    private MediaProjection projection;
    private MediaProjection.Callback projectionCallback;
    private SenderProfile activeProfile;

    ScreenSender(Context context, Listener listener) {
        this.listener = listener;
        this.audioSender = new AudioSender(context, this::reportFailure);
    }

    synchronized void start(MediaProjection projection, List<DiscoveryAgent.Peer> peers, boolean sendAudio) throws Exception {
        stop();
        failureReported.set(false);
        this.projection = projection;
        ArrayList<DiscoveryAgent.Peer> targets = new ArrayList<>(peers);

        try {
            projectionCallback = new MediaProjection.Callback() {
                @Override
                public void onStop() {
                    if (running.getAndSet(false)) {
                        AppLog.info("media projection was stopped by the system");
                        listener.onProjectionStopped();
                    }
                }
            };
            projection.registerCallback(projectionCallback, mainHandler);

            encoder = MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_VIDEO_AVC);
            SenderProfile requestedProfile = SenderProfile.fromPeers(targets);
            SenderProfile profile = requestedProfile.clampToEncoder(encoder);
            MediaFormat format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, profile.width, profile.height);
            format.setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface);
            format.setInteger(MediaFormat.KEY_BIT_RATE, profile.bitrate);
            format.setInteger(MediaFormat.KEY_FRAME_RATE, profile.fps);
            format.setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 1);
            if (android.os.Build.VERSION.SDK_INT >= 30) {
                format.setInteger(MediaFormat.KEY_LOW_LATENCY, 1);
            }
            if (android.os.Build.VERSION.SDK_INT >= 29) {
                format.setInteger(MediaFormat.KEY_PREPEND_HEADER_TO_SYNC_FRAMES, 1);
            }
            encoder.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
            inputSurface = encoder.createInputSurface();
            encoder.start();
            virtualDisplay = projection.createVirtualDisplay(
                    "screen-mirror",
                    profile.width,
                    profile.height,
                    DPI,
                    DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
                    inputSurface,
                    null,
                    null
            );
            if (virtualDisplay == null) {
                throw new IllegalStateException("failed to create capture virtual display");
            }
            activeProfile = profile;
            if (sendAudio) {
                audioSender.start(projection, targets);
            }
            running.set(true);
            thread = new Thread(() -> drainLoop(targets), "screen-sender");
            thread.start();
            AppLog.info("screen sender started " + profileDescription() + " bitrate=" + profile.bitrate);
        } catch (Exception error) {
            stop();
            throw error;
        }
    }

    String profileDescription() {
        SenderProfile profile = activeProfile;
        if (profile == null) {
            return DEFAULT_WIDTH + "x" + DEFAULT_HEIGHT + "@" + DEFAULT_FPS;
        }
        return profile.width + "x" + profile.height + "@" + profile.fps;
    }

    boolean isRunning() {
        return running.get();
    }

    synchronized void stop() {
        running.set(false);
        packetizer.close();
        audioSender.stop();

        Thread worker = thread;
        thread = null;
        joinWorker(worker);

        VirtualDisplay activeDisplay = virtualDisplay;
        virtualDisplay = null;
        if (activeDisplay != null) {
            activeDisplay.release();
        }

        Surface activeSurface = inputSurface;
        inputSurface = null;
        if (activeSurface != null) {
            activeSurface.release();
        }

        MediaCodec activeEncoder = encoder;
        encoder = null;
        if (activeEncoder != null) {
            try {
                activeEncoder.stop();
            } catch (Exception error) {
                AppLog.warn("video encoder stop failed", error);
            }
            activeEncoder.release();
        }
        activeProfile = null;

        MediaProjection activeProjection = projection;
        MediaProjection.Callback activeCallback = projectionCallback;
        projection = null;
        projectionCallback = null;
        if (activeProjection != null) {
            if (activeCallback != null) {
                activeProjection.unregisterCallback(activeCallback);
            }
            try {
                activeProjection.stop();
            } catch (Exception error) {
                AppLog.warn("media projection stop failed", error);
            }
        }
    }

    private void drainLoop(List<DiscoveryAgent.Peer> peers) {
        MediaCodec.BufferInfo info = new MediaCodec.BufferInfo();
        try {
            while (running.get()) {
                MediaCodec activeEncoder = encoder;
                if (activeEncoder == null) {
                    return;
                }
                int index = activeEncoder.dequeueOutputBuffer(info, 10_000);
                if (index >= 0) {
                    try {
                        ByteBuffer output = activeEncoder.getOutputBuffer(index);
                        if (output != null && info.size > 0) {
                            output.position(info.offset);
                            output.limit(info.offset + info.size);
                            packetizer.sendH264(output.slice(), info.size, info.presentationTimeUs, peers);
                        }
                    } finally {
                        activeEncoder.releaseOutputBuffer(index, false);
                    }
                }
            }
        } catch (Exception error) {
            if (running.get()) {
                running.set(false);
                reportFailure(error);
            }
        }
    }

    private void reportFailure(Throwable error) {
        if (!failureReported.compareAndSet(false, true)) {
            return;
        }
        AppLog.error("screen sender failed", error);
        mainHandler.post(() -> listener.onError(error));
    }

    private static void joinWorker(Thread worker) {
        if (worker == null || worker == Thread.currentThread()) {
            return;
        }
        worker.interrupt();
        try {
            worker.join(1000L);
            if (worker.isAlive()) {
                AppLog.warn("screen sender thread did not stop within one second", null);
            }
        } catch (InterruptedException error) {
            Thread.currentThread().interrupt();
            AppLog.warn("interrupted while stopping screen sender", error);
        }
    }

    static final class SenderProfile {
        final int width;
        final int height;
        final int fps;
        final int bitrate;

        SenderProfile(int width, int height, int fps) {
            this.width = width;
            this.height = height;
            this.fps = fps;
            this.bitrate = bitrate(width, height, fps);
        }

        static SenderProfile fromPeers(List<DiscoveryAgent.Peer> peers) {
            int bestWidth = DEFAULT_WIDTH;
            int bestHeight = DEFAULT_HEIGHT;
            int bestRefresh = DEFAULT_FPS;
            long bestArea = 0;
            for (int i = 0; i < peers.size(); i++) {
                DiscoveryAgent.Peer peer = peers.get(i);
                int width = normalizeDimension(peer.displayWidth, DEFAULT_WIDTH, MAX_WIDTH);
                int height = normalizeDimension(peer.displayHeight, DEFAULT_HEIGHT, MAX_HEIGHT);
                long area = (long) width * height;
                if (area > bestArea) {
                    bestArea = area;
                    bestWidth = width;
                    bestHeight = height;
                    bestRefresh = peer.refreshHz;
                }
            }
            return new SenderProfile(
                    alignDown(bestWidth, 2),
                    alignDown(bestHeight, 2),
                    clamp(bestRefresh <= 0 ? DEFAULT_FPS : bestRefresh, MIN_FPS, MAX_FPS)
            );
        }

        SenderProfile clampToEncoder(MediaCodec codec) {
            try {
                MediaCodecInfo.CodecCapabilities capabilities =
                        codec.getCodecInfo().getCapabilitiesForType(MediaFormat.MIMETYPE_VIDEO_AVC);
                MediaCodecInfo.VideoCapabilities videoCapabilities = capabilities.getVideoCapabilities();
                int widthAlignment = Math.max(2, videoCapabilities.getWidthAlignment());
                int heightAlignment = Math.max(2, videoCapabilities.getHeightAlignment());
                Range<Integer> supportedWidths = videoCapabilities.getSupportedWidths();
                int alignedWidth = alignDown(
                        clamp(width, supportedWidths.getLower(), Math.min(supportedWidths.getUpper(), MAX_WIDTH)),
                        widthAlignment
                );
                Range<Integer> supportedHeights = videoCapabilities.getSupportedHeightsFor(alignedWidth);
                int alignedHeight = alignDown(
                        clamp(height, supportedHeights.getLower(), Math.min(supportedHeights.getUpper(), MAX_HEIGHT)),
                        heightAlignment
                );
                int alignedFps = fps;
                if (!videoCapabilities.areSizeAndRateSupported(alignedWidth, alignedHeight, alignedFps)) {
                    alignedFps = DEFAULT_FPS;
                }
                if (!videoCapabilities.areSizeAndRateSupported(alignedWidth, alignedHeight, alignedFps)) {
                    alignedFps = MIN_FPS;
                }
                return new SenderProfile(alignedWidth, alignedHeight, alignedFps);
            } catch (Exception error) {
                AppLog.warn("could not query video encoder capabilities; using safe profile", error);
                return new SenderProfile(alignDown(width, 2), alignDown(height, 2), fps);
            }
        }

        private static int normalizeDimension(int value, int fallback, int max) {
            if (value <= 0) {
                return fallback;
            }
            return clamp(value, 320, max);
        }

        private static int bitrate(int width, int height, int fps) {
            long scaled = ((long) width * height * fps * 16L) / 100L;
            return clamp((int) Math.min(Integer.MAX_VALUE, scaled), MIN_BITRATE, MAX_BITRATE);
        }

        private static int alignDown(int value, int alignment) {
            int aligned = value - (value % alignment);
            return Math.max(alignment, aligned);
        }

        private static int clamp(int value, int min, int max) {
            return Math.max(min, Math.min(value, max));
        }
    }
}
