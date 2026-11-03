package dev.uxdroid.system;

import android.media.AudioAttributes;
import android.media.AudioManager;
import android.media.IAudioFocusDispatcher;
import android.media.PlayerBase;
import android.media.audiopolicy.IAudioPolicyCallback;
import android.os.IBinder;
import java.util.concurrent.atomic.AtomicInteger;

/**
 * The handler of IAudioService for one app: each player and recorder gets an id, every stream plays
 * at full volume, the ringer and mode are normal, and audio focus is granted, since the host's sound
 * server mixes all apps. Volume changes and focus loss come from the host and are not modeled yet.
 */
public final class AudioService {
    /** STREAM_MUSIC's steps on a phone. */
    static final int MAX_VOLUME = 15;

    private final AtomicInteger ids = new AtomicInteger(1);

    public int trackPlayer(PlayerBase.PlayerIdCard player) {
        return ids.getAndIncrement();
    }

    public int trackRecorder(IBinder recorder) {
        return ids.getAndIncrement();
    }

    public int getStreamVolume(int stream) {
        return MAX_VOLUME;
    }

    public int getStreamMaxVolume(int stream) {
        return MAX_VOLUME;
    }

    public int getStreamMinVolume(int stream) {
        return 0;
    }

    public int getRingerModeExternal() {
        return AudioManager.RINGER_MODE_NORMAL;
    }

    public int getRingerModeInternal() {
        return AudioManager.RINGER_MODE_NORMAL;
    }

    public int requestAudioFocus(AudioAttributes attributes, int gain, IBinder binder, IAudioFocusDispatcher dispatcher,
            String clientId, String callingPackage, String attributionTag, int flags, IAudioPolicyCallback policy,
            int sdk) {
        return AudioManager.AUDIOFOCUS_REQUEST_GRANTED;
    }

    public int abandonAudioFocus(IAudioFocusDispatcher dispatcher, String clientId, AudioAttributes attributes,
            String callingPackage) {
        return AudioManager.AUDIOFOCUS_REQUEST_GRANTED;
    }
}
