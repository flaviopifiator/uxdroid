package com.example.compat;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;

/** Started for a result: answers at once and finishes, as a picker or a login screen does. */
public class ResultActivity extends Activity {
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        int question = getIntent().getIntExtra("question", 0);
        setResult(RESULT_OK, new Intent().putExtra("answer", question * 2));
        finish();
    }
}
