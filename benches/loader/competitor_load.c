// Loads one library through bionic_translation's linker and exits.
#include <dlfcn.h>
#include <stdio.h>

void dl_parse_library_path(const char *path, char *delimiter);
void *bionic_dlopen(const char *filename, int flag);
const char *bionic_dlerror(void);

int main(int argc, char **argv) {
    if (argc != 3) return 2;
    dl_parse_library_path(argv[2], ":");
    if (bionic_dlopen(argv[1], RTLD_NOW) == NULL) {
        fprintf(stderr, "%s\n", bionic_dlerror());
        return 1;
    }
    return 0;
}
