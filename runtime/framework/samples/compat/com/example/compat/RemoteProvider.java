package com.example.compat;

/** Declared for the process :remote. */
public class RemoteProvider extends android.content.ContentProvider {
    @Override
    public boolean onCreate() {
        System.out.println("provider created RemoteProvider after application onCreate " + CompatApplication.created);
        return true;
    }
    /** One row naming the provider that answered. */
    @Override
    public android.database.Cursor query(android.net.Uri uri, String[] projection, String selection,
            String[] selectionArgs, String sortOrder) {
        android.database.MatrixCursor cursor = new android.database.MatrixCursor(new String[] {"provider"});
        cursor.addRow(new Object[] {getClass().getSimpleName()});
        return cursor;
    }

    @Override public String getType(android.net.Uri uri) { return null; }
    @Override public android.net.Uri insert(android.net.Uri uri, android.content.ContentValues values) { return null; }
    @Override public int delete(android.net.Uri uri, String selection, String[] selectionArgs) { return 0; }
    @Override public int update(android.net.Uri uri, android.content.ContentValues values, String selection,
            String[] selectionArgs) { return 0; }
}
