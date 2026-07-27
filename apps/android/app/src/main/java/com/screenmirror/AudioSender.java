package com.screenmirror;

import android.Manifest;
import android.content.Context;
import android.content.pm.PackageManager;
import android.media.AudioAttributes;
import android.media.AudioFormat;
import android.media.AudioPlaybackCaptureConfiguration;
import android.media.AudioRecord;
import android.media.MediaCodec;
import android.media.MediaFormat;
import android.media.projection.MediaProjection;

import java.nio.ByteBuffer;
import java.util.List;
import java.util.concurrent.atomic.AtomicBoolean;

final class AudioSender {
    interface Listener {
        void onError(Throwable error);
    }

    private static final int SAMPLE_RATE = 48_000;
    private static final int CHANNELS = 2;
    private static final int BITRATE = 96_000;
    static final int FRAME_SAMPLES = SAMPLE_RATE / 200;
    static final int PCM_FRAME_BYTES = FRAME_SAMPLES * CHANNELS * 2;

    private final Context context;
    private final Listener listener;
    private final AtomicBoolean running = new AtomicBoolean(false);
    private final RtpOpusPacketizer packetizer = new RtpOpusPacketizer();
    private final byte[] pcmBuffer = new byte[PCM_FRAME_BYTES];
    private final MediaCodec.BufferInfo bufferInfo = new MediaCodec.BufferInfo();

    private Thread thread;
    private AudioRecord audioRecord;
    private MediaCodec encoder;

    AudioSender(Context context, Listener listener) {
        this.context = context.getApplicationContext();
        this.listener = listener;
    }

    boolean isSupported() {
        return android.os.Build.VERSION.SDK_INT >= 29;
    }

    boolean isRunning() {
        return running.get();
    }

    synchronized void start(MediaProjection projection, List<DiscoveryAgent.Peer> peers) throws Exception {
        stop();
        if (!isSupported()) {
            throw new IllegalStateException("Android audio capture requires Android 10 or newer");
        }
        if (context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
            throw new SecurityException("record audio permission is not granted");
        }

        AudioRecord localRecord = null;
        MediaCodec localEncoder = null;
        try {
            AudioPlaybackCaptureConfiguration capture = new AudioPlaybackCaptureConfiguration.Builder(projection)
                    .addMatchingUsage(AudioAttributes.USAGE_MEDIA)
                    .addMatchingUsage(AudioAttributes.USAGE_GAME)
                    .addMatchingUsage(AudioAttributes.USAGE_UNKNOWN)
                    .build();

            AudioFormat audioFormat = new AudioFormat.Builder()
                    .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                    .setSampleRate(SAMPLE_RATE)
                    .setChannelMask(AudioFormat.CHANNEL_IN_STEREO)
                    .build();
            int minBuffer = AudioRecord.getMinBufferSize(
                    SAMPLE_RATE,
                    AudioFormat.CHANNEL_IN_STEREO,
                    AudioFormat.ENCODING_PCM_16BIT
            );
            int recordBuffer = Math.max(minBuffer, PCM_FRAME_BYTES * 4);
            localRecord = new AudioRecord.Builder()
                    .setAudioFormat(audioFormat)
                    .setBufferSizeInBytes(recordBuffer)
                    .setAudioPlaybackCaptureConfig(capture)
                    .build();
            if (localRecord.getState() != AudioRecord.STATE_INITIALIZED) {
                throw new IllegalStateException("audio capture did not initialize");
            }

            localEncoder = MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_AUDIO_OPUS);
            MediaFormat format = MediaFormat.createAudioFormat(MediaFormat.MIMETYPE_AUDIO_OPUS, SAMPLE_RATE, CHANNELS);
            format.setInteger(MediaFormat.KEY_BIT_RATE, BITRATE);
            format.setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, PCM_FRAME_BYTES);
            localEncoder.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
            localEncoder.start();

            localRecord.startRecording();
        } catch (Exception error) {
            if (localRecord != null) {
                localRecord.release();
            }
            if (localEncoder != null) {
                try {
                    localEncoder.stop();
                } catch (Exception stopError) {
                    AppLog.warn("audio encoder cleanup failed", stopError);
                }
                localEncoder.release();
            }
            throw error;
        }

        audioRecord = localRecord;
        encoder = localEncoder;
        running.set(true);
        thread = new Thread(() -> captureLoop(peers), "audio-sender");
        thread.start();
        AppLog.info(
                "sender audio capture started with "
                        + localRecord.getBufferSizeInFrames()
                        + "-frame buffer"
        );
    }

    synchronized void stop() {
        running.set(false);
        packetizer.close();

        AudioRecord activeRecord = audioRecord;
        if (activeRecord != null) {
            try {
                activeRecord.stop();
            } catch (Exception error) {
                AppLog.warn("audio capture stop failed", error);
            }
        }

        Thread worker = thread;
        thread = null;
        joinWorker(worker);

        audioRecord = null;
        if (activeRecord != null) {
            activeRecord.release();
        }

        MediaCodec activeEncoder = encoder;
        encoder = null;
        if (activeEncoder != null) {
            try {
                activeEncoder.stop();
            } catch (Exception error) {
                AppLog.warn("audio encoder stop failed", error);
            }
            activeEncoder.release();
        }
    }

    private void captureLoop(List<DiscoveryAgent.Peer> peers) {
        long samplesCaptured = 0;
        try {
            while (running.get()) {
                AudioRecord activeRecord = audioRecord;
                MediaCodec activeEncoder = encoder;
                if (activeRecord == null || activeEncoder == null) {
                    return;
                }

                int bytesRead = activeRecord.read(pcmBuffer, 0, pcmBuffer.length, AudioRecord.READ_BLOCKING);
                if (bytesRead <= 0) {
                    if (bytesRead < 0 && running.get()) {
                        throw new IllegalStateException("AudioRecord read failed with code " + bytesRead);
                    }
                    continue;
                }

                int input = activeEncoder.dequeueInputBuffer(1_000);
                if (input >= 0) {
                    ByteBuffer buffer = activeEncoder.getInputBuffer(input);
                    long presentationTimeUs = samplesCaptured * 1_000_000L / SAMPLE_RATE;
                    if (buffer == null) {
                        activeEncoder.queueInputBuffer(input, 0, 0, presentationTimeUs, 0);
                    } else {
                        buffer.clear();
                        buffer.put(pcmBuffer, 0, bytesRead);
                        activeEncoder.queueInputBuffer(input, 0, bytesRead, presentationTimeUs, 0);
                    }
                }
                samplesCaptured += bytesRead / (CHANNELS * 2);
                drainEncoder(activeEncoder, peers);
            }
        } catch (Exception error) {
            if (running.get()) {
                running.set(false);
                AppLog.error("sender audio capture failed", error);
                listener.onError(error);
            }
        }
    }

    private void drainEncoder(MediaCodec activeEncoder, List<DiscoveryAgent.Peer> peers) throws Exception {
        int output;
        do {
            output = activeEncoder.dequeueOutputBuffer(bufferInfo, 0);
            if (output >= 0) {
                try {
                    ByteBuffer encoded = activeEncoder.getOutputBuffer(output);
                    if (encoded != null && bufferInfo.size > 0) {
                        encoded.position(bufferInfo.offset);
                        encoded.limit(bufferInfo.offset + bufferInfo.size);
                        packetizer.sendOpus(encoded.slice(), bufferInfo.size, bufferInfo.presentationTimeUs, peers);
                    }
                } finally {
                    activeEncoder.releaseOutputBuffer(output, false);
                }
            }
        } while (output >= 0);
    }

    private static void joinWorker(Thread worker) {
        if (worker == null || worker == Thread.currentThread()) {
            return;
        }
        worker.interrupt();
        try {
            worker.join(1000L);
            if (worker.isAlive()) {
                AppLog.warn("audio sender thread did not stop within one second", null);
            }
        } catch (InterruptedException error) {
            Thread.currentThread().interrupt();
            AppLog.warn("interrupted while stopping audio sender", error);
        }
    }
}
