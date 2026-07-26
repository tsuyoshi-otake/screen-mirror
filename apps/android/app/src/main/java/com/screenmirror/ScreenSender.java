package com.screenmirror;

import android.hardware.display.DisplayManager;
import android.hardware.display.VirtualDisplay;
import android.media.MediaCodec;
import android.media.MediaCodecInfo;
import android.media.MediaFormat;
import android.media.projection.MediaProjection;
import android.view.Surface;

import java.nio.ByteBuffer;
import java.util.List;
import java.util.concurrent.atomic.AtomicBoolean;

final class ScreenSender {
    private static final int WIDTH = 1280;
    private static final int HEIGHT = 720;
    private static final int DPI = 320;
    private static final int FPS = 30;
    private static final int BITRATE = 8_000_000;

    private final AtomicBoolean running = new AtomicBoolean(false);
    private final RtpPacketizer packetizer = new RtpPacketizer();
    private Thread thread;
    private MediaCodec encoder;
    private VirtualDisplay virtualDisplay;
    private MediaProjection projection;

    void start(MediaProjection projection, List<DiscoveryAgent.Peer> peers) throws Exception {
        stop();
        this.projection = projection;
        encoder = MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_VIDEO_AVC);
        MediaFormat format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, WIDTH, HEIGHT);
        format.setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface);
        format.setInteger(MediaFormat.KEY_BIT_RATE, BITRATE);
        format.setInteger(MediaFormat.KEY_FRAME_RATE, FPS);
        format.setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 1);
        if (android.os.Build.VERSION.SDK_INT >= 30) {
            format.setInteger(MediaFormat.KEY_LOW_LATENCY, 1);
        }
        if (android.os.Build.VERSION.SDK_INT >= 23) {
            format.setInteger(MediaFormat.KEY_PREPEND_HEADER_TO_SYNC_FRAMES, 1);
        }
        encoder.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
        Surface inputSurface = encoder.createInputSurface();
        encoder.start();
        virtualDisplay = projection.createVirtualDisplay(
                "screen-mirror",
                WIDTH,
                HEIGHT,
                DPI,
                DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
                inputSurface,
                null,
                null
        );
        running.set(true);
        thread = new Thread(() -> drainLoop(peers), "screen-sender");
        thread.start();
    }

    void stop() {
        running.set(false);
        if (thread != null) {
            thread.interrupt();
            thread = null;
        }
        if (virtualDisplay != null) {
            virtualDisplay.release();
            virtualDisplay = null;
        }
        if (encoder != null) {
            try {
                encoder.stop();
            } catch (Exception ignored) {
            }
            encoder.release();
            encoder = null;
        }
        packetizer.close();
        if (projection != null) {
            projection.stop();
            projection = null;
        }
    }

    private void drainLoop(List<DiscoveryAgent.Peer> peers) {
        MediaCodec.BufferInfo info = new MediaCodec.BufferInfo();
        while (running.get()) {
            try {
                int index = encoder.dequeueOutputBuffer(info, 10_000);
                if (index >= 0) {
                    ByteBuffer output = encoder.getOutputBuffer(index);
                    if (output != null && info.size > 0) {
                        output.position(info.offset);
                        output.limit(info.offset + info.size);
                        packetizer.sendH264(output.slice(), info.size, info.presentationTimeUs, peers);
                    }
                    encoder.releaseOutputBuffer(index, false);
                }
            } catch (Exception ignored) {
            }
        }
    }
}
