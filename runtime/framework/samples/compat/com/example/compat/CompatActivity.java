package com.example.compat;

import android.app.Activity;
import android.os.Bundle;
import android.widget.TextView;

/** The launcher activity: a line of text, so the app has a window like the corpus apps. */
public class CompatActivity extends Activity {
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        TextView text = new TextView(this);
        text.setText("compat");
        setContentView(text);
        System.out.println("activity created " + getClass().getName());
        // An app starts its own activities, by class and by action, as Markor's and AnkiDroid's
        // first activities do; an intent no app takes fails as on Android.
        startActivity(new android.content.Intent(this, SecondActivity.class).putExtra("by", "class"));
        startActivity(new android.content.Intent("com.example.compat.SECOND").putExtra("by", "action"));
        pendingIntents();
        // A result comes back to the activity that asked, as an app's picker or login screen answers.
        startActivityForResult(new android.content.Intent(this, ResultActivity.class).putExtra("question", 21), 7);
        try {
            startActivity(new android.content.Intent(android.content.Intent.ACTION_VIEW,
                    android.net.Uri.parse("https://example.org/")));
            System.out.println("started an intent no activity takes");
        } catch (android.content.ActivityNotFoundException e) {
            System.out.println("no activity takes " + e.getMessage().replaceAll(" \\{.*", ""));
        }
        // From another thread, the answer still comes from the task, not a blind success.
        Thread start = new Thread(() -> {
            try {
                startActivity(new android.content.Intent("com.example.compat.NOBODY"));
                System.out.println("started off the main thread an intent no activity takes");
            } catch (android.content.ActivityNotFoundException e) {
                System.out.println("off the main thread, no activity takes it");
            }
        });
        start.start();
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, android.content.Intent data) {
        System.out.println("result request " + requestCode + " code " + resultCode + " answer "
                + (data == null ? "none" : data.getIntExtra("answer", -1)));
    }

    /** Loop Habit Tracker makes PendingIntents for its reminders and widgets. */
    private void pendingIntents() {
        android.content.Intent reminder = new android.content.Intent("com.example.compat.REMIND").setPackage(getPackageName());
        android.app.PendingIntent first = android.app.PendingIntent.getBroadcast(this, 1, reminder,
                android.app.PendingIntent.FLAG_IMMUTABLE);
        android.app.PendingIntent again = android.app.PendingIntent.getBroadcast(this, 1,
                new android.content.Intent(reminder).putExtra("habit", 3), android.app.PendingIntent.FLAG_IMMUTABLE);
        android.app.PendingIntent other = android.app.PendingIntent.getBroadcast(this, 2, reminder,
                android.app.PendingIntent.FLAG_IMMUTABLE | android.app.PendingIntent.FLAG_NO_CREATE);
        System.out.println("pending broadcast " + (first != null) + " same " + first.equals(again)
                + " creator " + first.getCreatorPackage() + " absent " + (other == null));
        // The flags a PendingIntent is made with are part of its identity, as PendingIntentRecord's key;
        // a canceled one is not sent.
        android.app.PendingIntent shot = android.app.PendingIntent.getBroadcast(this, 1, reminder,
                android.app.PendingIntent.FLAG_IMMUTABLE | android.app.PendingIntent.FLAG_ONE_SHOT);
        shot.cancel();
        String canceled;
        try {
            shot.send();
            canceled = "sent";
        } catch (android.app.PendingIntent.CanceledException e) {
            canceled = "refused";
        }
        System.out.println("pending flags apart " + !shot.equals(first) + " first kept "
                + (android.app.PendingIntent.getBroadcast(this, 1, reminder, android.app.PendingIntent.FLAG_IMMUTABLE
                        | android.app.PendingIntent.FLAG_NO_CREATE) != null) + " canceled " + canceled);
        try {
            android.app.PendingIntent.getActivity(this, 0,
                    new android.content.Intent(this, SecondActivity.class).putExtra("by", "pending intent"),
                    android.app.PendingIntent.FLAG_IMMUTABLE).send();
        } catch (android.app.PendingIntent.CanceledException e) {
            System.out.println("pending activity canceled");
        }
    }
}
