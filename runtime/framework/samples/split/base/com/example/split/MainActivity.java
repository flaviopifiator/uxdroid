package com.example.split;

import android.app.Activity;
import android.content.Context;
import android.os.Bundle;
import android.widget.TextView;
import java.util.Arrays;

/** Uses code and a resource that only its feature split holds, as an app reaches an installed feature. */
public class MainActivity extends Activity {
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        TextView text = new TextView(this);
        System.out.println("splits " + Arrays.toString(getApplicationInfo().splitNames));
        try {
            // The feature's class is in split_feature.apk, which Android adds to this app's class loader.
            Class<?> feature = Class.forName("com.example.split.feature.Feature");
            String said = (String) feature.getMethod("describe", Context.class).invoke(null, this);
            text.setText(said);
            System.out.println("split code " + feature.getName() + " says \"" + said + "\"");
        } catch (ReflectiveOperationException | RuntimeException e) {
            text.setText("no feature");
            System.out.println("split code missing: " + e);
        }
        setContentView(text);
    }
}
