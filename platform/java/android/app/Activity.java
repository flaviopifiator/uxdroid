package android.app;

import android.content.Context;
import android.os.Bundle;
import android.view.View;

/** One screen of an app. The runtime drives its lifecycle. */
public class Activity extends Context {
    private View contentView;
    private boolean finishing;
    private boolean finishedDuringCreate;

    protected void onCreate(Bundle savedInstanceState) {}

    protected void onStart() {}

    protected void onResume() {}

    protected void onPause() {}

    protected void onStop() {}

    protected void onDestroy() {}

    public void setContentView(View view) {
        contentView = view;
    }

    public View getContentView() {
        return contentView;
    }

    public void finish() {
        finishing = true;
    }

    public boolean isFinishing() {
        return finishing;
    }

    @Override
    public String getPackageName() {
        return System.getProperty("uxdroid.package", "");
    }

    /** Called by the runtime, because lifecycle methods are protected. */
    public final void dispatchLifecycle(int stage) {
        switch (stage) {
            case 0:
                onCreate(null);
                // As on Android, finish() in onCreate skips straight to onDestroy.
                finishedDuringCreate = finishing;
                if (!finishing) {
                    onStart();
                    onResume();
                }
                break;
            default:
                if (!finishedDuringCreate) {
                    onPause();
                    onStop();
                }
                onDestroy();
                break;
        }
    }
}
