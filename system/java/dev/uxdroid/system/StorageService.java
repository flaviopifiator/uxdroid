package dev.uxdroid.system;

import android.os.Environment;
import android.os.UserHandle;
import android.os.storage.StorageVolume;
import java.io.File;

/**
 * The handler of IStorageManager: one emulated primary volume, the shared storage of user 0. Android
 * backs it with {@code /data/media/0}; here it is the same directory under ANDROID_DATA, so an app's
 * external files directory is real and writable.
 */
public final class StorageService {
    private final StorageVolume primary;

    StorageService(String dataRoot) {
        File shared = new File(dataRoot, "media/0");
        if (!shared.isDirectory() && !shared.mkdirs()) {
            throw new IllegalStateException("cannot create " + shared);
        }
        primary = new StorageVolume("emulated;0", shared, shared, "Internal shared storage",
                true, false, true, false, // primary, not removable, emulated, not externally removable
                false, 0, UserHandle.of(0), // no mass storage, no file size limit
                null, null, Environment.MEDIA_MOUNTED);
    }

    public StorageVolume[] getVolumeList(int userId, String callingPackage, int flags) {
        return userId == 0 ? new StorageVolume[] {primary} : new StorageVolume[0];
    }
}
