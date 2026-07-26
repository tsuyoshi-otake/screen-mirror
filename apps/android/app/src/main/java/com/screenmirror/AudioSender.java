package com.screenmirror;

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
    private static final int SAMPLE_RATE = 48_000;
    private static final int CHANNELS = 2;
    private static final int BITRATE = 96_000;
    private static final int FRAME_MS = 5;
    private static final int PCM_FRAME_BYTES = SAMPLE_RATE * CHANNELS * 2 * FRAME_MS / 1000;

    private final AtomicBoolean running = new AtomicBoolean(false);
    private final RtpOpusPacketizer packetizer = new RtpOpusPacketizer();
    private final byte[] pcmBuffer = new byte[PCM_FRAME_BYTES];
    private final MediaCodec.BufferInfo bufferInfo = new MediaCodec.BufferInfo();

    private Thread thread;
    private AudioRecord audioRecord;
    private MediaCodec encoder;

    boolean isSupported() {
        return android.os.Build.VERSION.SDK_INT >= 29;
    }

    void start(MediaProjection projection, List<DiscoveryAgent.Peer> peers) throws Exception {
        stop();
        if (!isSupported()) {
            throw new IllegalStateException("Android audio capture requires Android 10 or newer");
        }

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
        audioRecord = new AudioRecord.Builder()
                .setAudioFormat(audioFormat)
                .setBufferSizeInBytes(recordBuffer)
                .setAudioPlaybackCaptureConfig(capture)
                .build();

        encoder = MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_AUDIO_OPUS);
        MediaFormat format = MediaFormat.createAudioFormat(MediaFormat.MIMETYPE_AUDIO_OPUS, SAMPLE_RATE, CHANNELS);
        format.setInteger(MediaFormat.KEY_BIT_RATE, BITRATE);
        format.setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, PCM_FRAME_BYTES);
        encoder.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
        encoder.start();

        audioRecord.startRecording();
        running.set(true);
        thread = new Thread(() -> captureLoop(peers), "audio-sender");
        thread.start();
    }

    void stop() {
        running.set(false);
        if (thread != null) {
            thread.interrupt();
            thread = null;
        }
        if (audioRecord != null) {
            try {
                audioRecord.stop();
            } catch (Exception ignored) {
            }
            audioRecord.release();
            audioRecord = null;
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
    }

    private void captureLoop(List<DiscoveryAgent.Peer> peers) {
        long samplesCaptured = 0;
        while (running.get()) {
            try {
                AudioRecord activeRecord = audioRecord;
                MediaCodec activeEncoder = encoder;
                if (activeRecord == null || activeEncoder == null) {
                    return;
                }

                int bytesRead = activeRecord.read(pcmBuffer, 0, pcmBuffer.length, AudioRecord.READ_BLOCKING);
                if (bytesRead <= 0) {
                    continue;
                }

                int input = activeEncoder.dequeueInputBuffer(1_000);
                if (input >= 0) {
                    ByteBuffer buffer = activeEncoder.getInputBuffer(input);
                    if (buffer != null) {
                        buffer.clear();
                        buffer.put(pcmBuffer, 0, bytesRead);
                        long presentationTimeUs = samplesCaptured * 1_000_000L / SAMPLE_RATE;
                        activeEncoder.queueInputBuffer(input, 0, bytesRead, presentationTimeUs, 0);
                    }
                }
                samplesCaptured += bytesRead / (CHANNELS * 2);
                drainEncoder(activeEncoder, peers);
            } catch (Exception ignored) {
            }
        }
    }

    private void drainEncoder(MediaCodec activeEncoder, List<DiscoveryAgent.Peer> peers) throws Exception {
        int output;
        do {
            output = activeEncoder.dequeueOutputBuffer(bufferInfo, 0);
            if (output >= 0) {
                ByteBuffer encoded = activeEncoder.getOutputBuffer(output);
                if (encoded != null && bufferInfo.size > 0) {
                    encoded.position(bufferInfo.offset);
                    encoded.limit(bufferInfo.offset + bufferInfo.size);
                    packetizer.sendOpus(encoded.slice(), bufferInfo.size, bufferInfo.presentationTimeUs, peers);
                }
                activeEncoder.releaseOutputBuffer(output, false);
            }
        } while (output >= 0);
    }
}
