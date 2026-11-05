package com.example.split.feature;

import android.content.Context;

/** Code of the feature split, reading a string from the split's own resources. */
public final class Feature {
    private Feature() {}

    public static String describe(Context context) {
        return context.getString(R.string.feature_text);
    }
}
