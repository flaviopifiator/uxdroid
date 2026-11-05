// A static arm64 Linux program around the kernel, for QEMU user: raw system calls only, so QEMU
// runs the kernel itself and nothing else. Prints the same JSON as kernel_bench.
//
// Usage: qemu-aarch64 kernel_qemu <MiB>
#include <stddef.h>
#include <stdint.h>

void kernel_fill(uint8_t* buffer, size_t size, uint64_t seed);
void kernel_sha256(const uint8_t* data, size_t size, uint8_t digest[32]);
size_t kernel_deflate(const uint8_t* data, size_t size, uint8_t* out, size_t out_size);

static long sys(long number, long a, long b, long c, long d, long e, long f) {
    register long x8 __asm__("x8") = number;
    register long x0 __asm__("x0") = a;
    register long x1 __asm__("x1") = b;
    register long x2 __asm__("x2") = c;
    register long x3 __asm__("x3") = d;
    register long x4 __asm__("x4") = e;
    register long x5 __asm__("x5") = f;
    __asm__ volatile("svc #0" : "+r"(x0) : "r"(x8), "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x5) : "memory");
    return x0;
}

enum { SYS_OPENAT = 56, SYS_CLOSE = 57, SYS_READ = 63, SYS_WRITE = 64, SYS_EXIT = 93, SYS_CLOCK = 113, SYS_MMAP = 222 };

static char out[4096];
static size_t used;

static void put(const char* text) {
    while (*text && used < sizeof(out)) out[used++] = *text++;
}

static void put_u(uint64_t value) {
    char digits[24];
    int n = 0;
    do { digits[n++] = (char)('0' + value % 10); value /= 10; } while (value);
    while (n) { char one[2] = {digits[--n], 0}; put(one); }
}

static void put_ms(uint64_t nanoseconds) {
    put_u(nanoseconds / 1000000);
    put(".");
    uint64_t micro = nanoseconds / 1000 % 1000;
    char three[4] = {(char)('0' + micro / 100), (char)('0' + micro / 10 % 10), (char)('0' + micro % 10), 0};
    put(three);
}

static uint64_t now(void) {
    long t[2];
    sys(SYS_CLOCK, 1 /* CLOCK_MONOTONIC */, (long)t, 0, 0, 0, 0);
    return (uint64_t)t[0] * 1000000000u + (uint64_t)t[1];
}

static uint64_t field_kib(const char* path, const char* field) {
    static char text[8192];
    long fd = sys(SYS_OPENAT, -100, (long)path, 0, 0, 0, 0);
    if (fd < 0) return 0;
    long size = sys(SYS_READ, fd, (long)text, sizeof(text) - 1, 0, 0, 0);
    sys(SYS_CLOSE, fd, 0, 0, 0, 0, 0);
    if (size <= 0) return 0;
    text[size] = 0;
    for (char* line = text; *line; ) {
        const char* f = field;
        char* at = line;
        while (*f && *at == *f) { ++f; ++at; }
        if (!*f) {
            while (*at == ' ' || *at == '\t') ++at;
            uint64_t value = 0;
            while (*at >= '0' && *at <= '9') value = value * 10 + (uint64_t)(*at++ - '0');
            return value;
        }
        while (*line && *line != '\n') ++line;
        if (*line) ++line;
    }
    return 0;
}

static void* map(size_t size) {
    return (void*)sys(SYS_MMAP, 0, (long)size, 3 /* RW */, 0x22 /* PRIVATE|ANONYMOUS */, -1, 0);
}

long qemu_main(long* stack) {
    long argc = stack[0];
    char** argv = (char**)(stack + 1);
    size_t mib = 64;
    if (argc > 1) {
        mib = 0;
        for (const char* c = argv[1]; *c; ++c) mib = mib * 10 + (size_t)(*c - '0');
    }
    size_t size = mib << 20;
    size_t out_size = size + size / 1000 + (1 << 20);
    uint8_t* input = map(size);
    uint8_t* output = map(out_size);

    uint64_t start = now();
    kernel_fill(input, size, 0x00C0FFEE);
    uint64_t fill = now() - start;
    uint8_t digest[32];
    uint64_t sha[2], deflate[2];
    size_t compressed = 0;
    for (int i = 0; i < 2; ++i) {
        start = now();
        kernel_sha256(input, size, digest);
        sha[i] = now() - start;
        start = now();
        compressed = kernel_deflate(input, size, output, out_size);
        deflate[i] = now() - start;
    }
    static const char hex[] = "0123456789abcdef";
    char text[65];
    for (int i = 0; i < 32; ++i) { text[i * 2] = hex[digest[i] >> 4]; text[i * 2 + 1] = hex[digest[i] & 15]; }
    text[64] = 0;
    put("{\"load_ms\":0.000,\"fill_ms\":"); put_ms(fill);
    put(",\"sha_ms\":["); put_ms(sha[0]); put(","); put_ms(sha[1]);
    put("],\"deflate_ms\":["); put_ms(deflate[0]); put(","); put_ms(deflate[1]);
    put("],\"digest\":\""); put(text);
    put("\",\"compressed\":"); put_u(compressed);
    put(",\"peak_rss_kib\":"); put_u(field_kib("/proc/self/status", "VmHWM:"));
    put(",\"private_dirty_kib\":"); put_u(field_kib("/proc/self/smaps_rollup", "Private_Dirty:"));
    put("}\n");
    sys(SYS_WRITE, 1, (long)out, (long)used, 0, 0, 0);
    return 0;
}

__attribute__((naked)) void _start(void) {
    __asm__("mov x0, sp\n"
            "bl qemu_main\n"
            "mov x8, #94\n" /* exit_group */
            "svc #0\n");
}
