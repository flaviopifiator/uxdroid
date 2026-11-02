// Loads one library through the system dynamic linker and exits: the native reference.
#include <dlfcn.h>
#include <stdio.h>

int main(int argc, char **argv) {
    if (argc != 2) return 2;
    if (dlopen(argv[1], RTLD_NOW) == NULL) {
        fprintf(stderr, "%s\n", dlerror());
        return 1;
    }
    return 0;
}
