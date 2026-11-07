// A static arm64 Linux program around the game kernel, for QEMU user: raw system calls only.
// It runs the compute phases, particles and matrices, and prints the same JSON as game_bench.
// host_calls and tick are left out: under QEMU user the guest's libc is guest code too, so there
// is no host crossing to compare.
//
// Usage: qemu-aarch64 games_qemu
#include <stddef.h>
#include <stdint.h>

uint64_t game_particles(uint32_t steps);
uint64_t game_matrices(uint32_t rounds);

// Only to link game_host_calls, which this program does not run.
size_t strlen(const char* text) {
    size_t size = 0;
    while (text[size]) ++size;
    return size;
}
float sinf(float value) { return value; }

static long sys(long number, long a, long b, long c) {
    register long x8 __asm__("x8") = number;
    register long x0 __asm__("x0") = a;
    register long x1 __asm__("x1") = b;
    register long x2 __asm__("x2") = c;
    __asm__ volatile("svc #0" : "+r"(x0) : "r"(x8), "r"(x1), "r"(x2) : "memory");
    return x0;
}

enum { SYS_WRITE = 64, SYS_CLOCK = 113 };

static char out[1024];
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

static void put_hex(uint64_t value) {
    static const char hex[] = "0123456789abcdef";
    char text[17];
    for (int i = 0; i < 16; ++i) text[i] = hex[(value >> (60 - i * 4)) & 15];
    text[16] = 0;
    put(text);
}

static uint64_t now(void) {
    long t[2];
    sys(SYS_CLOCK, 1 /* CLOCK_MONOTONIC */, (long)t, 0);
    return (uint64_t)t[0] * 1000000000u + (uint64_t)t[1];
}

static void phase(const char* name, uint64_t (*run)(uint32_t), uint32_t argument) {
    uint64_t times[2], sum = 0;
    for (int i = 0; i < 2; ++i) {
        uint64_t start = now();
        sum = run(argument);
        times[i] = now() - start;
    }
    put("\""); put(name); put("\":{\"ms\":["); put_ms(times[0]); put(","); put_ms(times[1]);
    put("],\"sum\":\""); put_hex(sum); put("\"},");
}

long qemu_main(void) {
    put("{");
    // The same arguments as game_bench.rs.
    phase("particles", game_particles, 20000);
    phase("matrices", game_matrices, 20000);
    put("\"host_calls\":null,\"tick\":null}\n");
    sys(SYS_WRITE, 1, (long)out, (long)used);
    return 0;
}

__attribute__((naked)) void _start(void) {
    __asm__("bl qemu_main\n"
            "mov x8, #94\n" /* exit_group */
            "svc #0\n");
}
