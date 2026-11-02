// What libraries built for API levels below 23 compile `stdout` and `stderr` into.
#include <stdio.h>

struct bionic_file {
    char opaque[152];
};
extern struct bionic_file __sF[];

#define LEGACY_STDOUT ((FILE *)&__sF[1])
#define LEGACY_STDERR ((FILE *)&__sF[2])

int test_legacy_streams(void) {
    if (fputs("legacy fputs\n", LEGACY_STDOUT) < 0) return 1;
    if (fwrite("legacy fwrite\n", 1, 14, LEGACY_STDOUT) != 14) return 2;
    if (fflush(LEGACY_STDOUT) != 0) return 3;
    if (fprintf(LEGACY_STDERR, "%s %d %.1f %d %d %d %d %d %d\n", "legacy", 1, 2.5, 3, 4, 5, 6, 7, 8) <= 0)
        return 4;
    if (fileno(LEGACY_STDERR) != 2) return 5;
    return 0;
}
