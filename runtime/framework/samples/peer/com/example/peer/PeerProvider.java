package com.example.peer;

import android.content.ContentValues;
import android.database.Cursor;
import android.net.Uri;

/** Declared to be found by other apps' queryIntentContentProviders; answers nothing. */
public class PeerProvider extends android.content.ContentProvider {
    @Override public boolean onCreate() { return true; }
    @Override public Cursor query(Uri uri, String[] projection, String selection, String[] args, String order) { return null; }
    @Override public String getType(Uri uri) { return null; }
    @Override public Uri insert(Uri uri, ContentValues values) { return null; }
    @Override public int delete(Uri uri, String selection, String[] args) { return 0; }
    @Override public int update(Uri uri, ContentValues values, String selection, String[] args) { return 0; }
}
