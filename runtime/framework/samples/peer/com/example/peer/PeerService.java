package com.example.peer;

/** Declared to be found by other apps' queryIntentServices; never bound by the smoke. */
public class PeerService extends android.app.Service {
    @Override
    public android.os.IBinder onBind(android.content.Intent intent) {
        return null;
    }
}
