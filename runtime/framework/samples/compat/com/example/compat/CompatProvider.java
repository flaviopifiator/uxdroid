package com.example.compat;

/** Installed when the app starts; prints what androidx-startup's provider reads in onCreate. */
public class CompatProvider extends android.content.ContentProvider {
    @Override
    public boolean onCreate() {
        try {
            android.content.pm.ProviderInfo info = getContext().getPackageManager().getProviderInfo(
                    new android.content.ComponentName(getContext(), CompatProvider.class),
                    android.content.pm.PackageManager.GET_META_DATA);
            System.out.println("provider created before application onCreate " + !CompatApplication.created
                    + " meta-data " + info.metaData.getString("com.example.compat.Initializer"));
        } catch (android.content.pm.PackageManager.NameNotFoundException e) {
            throw new IllegalStateException(e);
        }
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
