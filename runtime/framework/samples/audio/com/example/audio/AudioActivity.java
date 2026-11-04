package com.example.audio;

import android.app.Activity;
import android.media.AudioAttributes;
import android.media.AudioFormat;
import android.media.AudioManager;
import android.media.AudioRecord;
import android.media.AudioTrack;
import android.media.MediaRecorder;
import android.os.Bundle;

/**
 * Plays a tone and records through Java's AudioTrack and AudioRecord, then through OpenSL ES and
 * AAudio from native code. Every signal is integer arithmetic, so audio_smoke.sh can compute each
 * sample the device received and each sum the app recorded. Knows nothing about uxdroid.
 */
public class AudioActivity extends Activity {
    static {
        System.loadLibrary("audiosample");
    }

    /** OpenSL ES: a buffer-queue player at 44100 Hz stereo, then a recorder at 16000 Hz mono. */
    static native String openSl(int frames);

    /** AAudio: a float callback stream at 32000 Hz stereo, then a blocking 16-bit input stream. */
    static native String aaudio(int frames);

    static final int RATE = 48000;
    static final int FRAMES = 4800;

    /** A 440 Hz sawtooth from -32768, the same formula the smoke uses. */
    static short saw(int i, int hz, int rate) {
        return (short) ((((long) i * hz * 65536 / rate) & 0xFFFF) - 32768);
    }

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        new Thread(this::run, "audio sample").start();
    }

    private void run() {
        try {
            System.out.println("audiotrack: " + playTrack());
            System.out.println("audiorecord: " + record());
            System.out.println("opensl: " + openSl(FRAMES));
            System.out.println("aaudio: " + aaudio(FRAMES));
        } catch (Throwable t) {
            t.printStackTrace();
        }
        System.out.println("audio sample done");
    }

    private String playTrack() throws InterruptedException {
        AudioManager audio = getSystemService(AudioManager.class);
        String nativeRate = audio.getProperty(AudioManager.PROPERTY_OUTPUT_SAMPLE_RATE);
        String burst = audio.getProperty(AudioManager.PROPERTY_OUTPUT_FRAMES_PER_BUFFER);
        int min = AudioTrack.getMinBufferSize(RATE, AudioFormat.CHANNEL_OUT_MONO, AudioFormat.ENCODING_PCM_16BIT);
        AudioTrack track = new AudioTrack.Builder()
                .setAudioAttributes(new AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_MEDIA)
                        .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC).build())
                .setAudioFormat(new AudioFormat.Builder().setSampleRate(RATE)
                        .setChannelMask(AudioFormat.CHANNEL_OUT_MONO)
                        .setEncoding(AudioFormat.ENCODING_PCM_16BIT).build())
                .setBufferSizeInBytes(min * 4)
                .setTransferMode(AudioTrack.MODE_STREAM)
                .build();
        if (track.getState() != AudioTrack.STATE_INITIALIZED) return "not initialized";
        track.setNotificationMarkerPosition(1200);
        track.setPositionNotificationPeriod(480);
        System.out.println("audiotrack marker " + track.getNotificationMarkerPosition()
                + " period " + track.getPositionNotificationPeriod());
        short[] tone = new short[FRAMES];
        for (int i = 0; i < FRAMES; i++) tone[i] = saw(i, 440, RATE);
        track.play();
        int written = 0;
        // In slices, as a game loop writes.
        for (int at = 0; at < FRAMES; at += 960) {
            written += track.write(tone, at, Math.min(960, FRAMES - at));
        }
        track.stop();
        long deadline = System.currentTimeMillis() + 2000;
        while (track.getPlaybackHeadPosition() < FRAMES && System.currentTimeMillis() < deadline) {
            Thread.sleep(10);
        }
        String result = "rate " + nativeRate + ", burst " + burst + ", min buffer " + min + ", wrote " + written
                + ", played " + track.getPlaybackHeadPosition() + ", underruns " + track.getUnderrunCount()
                + ", sample rate " + track.getSampleRate();
        track.release();
        return result;
    }

    private String record() {
        int rate = 16000;
        int min = AudioRecord.getMinBufferSize(rate, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT);
        AudioRecord recorder = new AudioRecord(MediaRecorder.AudioSource.MIC, rate, AudioFormat.CHANNEL_IN_MONO,
                AudioFormat.ENCODING_PCM_16BIT, min * 4);
        if (recorder.getState() != AudioRecord.STATE_INITIALIZED) return "not initialized";
        short[] samples = new short[1600];
        recorder.startRecording();
        int read = 0;
        while (read < samples.length) {
            int got = recorder.read(samples, read, samples.length - read);
            if (got <= 0) break;
            read += got;
        }
        recorder.stop();
        recorder.release();
        long sum = 0;
        for (short sample : samples) sum += sample;
        return "read " + read + ", sum " + sum + ", first " + samples[0] + " " + samples[1] + " " + samples[2];
    }
}
