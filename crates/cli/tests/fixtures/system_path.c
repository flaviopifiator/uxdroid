// Reads a file of Android's system partition by its absolute path, through each function that takes
// one. Returns 0 when every call found the probe file, or the number of the first that did not.
#include <dirent.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <unistd.h>

#define PROBE "/system/etc/uxdroid-probe"

int read_system_probe(void) {
    FILE* file = fopen(PROBE, "r");
    if (file == NULL || fgetc(file) != '7') return 1;
    fclose(file);
    int fd = open(PROBE, O_RDONLY);
    if (fd < 0) return 2;
    close(fd);
    fd = openat(AT_FDCWD, PROBE, O_RDONLY);
    if (fd < 0) return 3;
    close(fd);
    struct stat info;
    if (stat(PROBE, &info) != 0 || info.st_size != 1) return 4;
    if (lstat(PROBE, &info) != 0) return 5;
    if (fstatat(AT_FDCWD, PROBE, &info, 0) != 0) return 6;
    if (access(PROBE, R_OK) != 0) return 7;
    DIR* directory = opendir("/system/etc");
    if (directory == NULL) return 8;
    closedir(directory);
    char* resolved = realpath(PROBE, NULL);
    if (resolved == NULL) return 9;
    free(resolved);
    return 0;
}
