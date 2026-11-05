// The translation benchmark's compute kernel: SHA-256 and zlib deflate over one buffer.
//
// Freestanding on purpose: it calls nothing outside itself and zlib, so the native x86_64
// build, the arm64 build run by the bridge and the arm64 build under QEMU execute the same code,
// and no host library call makes one of them faster. memcpy and its kind, which compilers may
// emit for struct copies, are defined here and hidden.
#include <stddef.h>
#include <stdint.h>

#include "zlib.h"

#define EXPORT __attribute__((visibility("default")))

// --- Memory the compiler may call ---

void* memcpy(void* to, const void* from, size_t size) {
    unsigned char* out = to;
    const unsigned char* in = from;
    while (size--) {
        *out++ = *in++;
    }
    return to;
}

void* memmove(void* to, const void* from, size_t size) {
    unsigned char* out = to;
    const unsigned char* in = from;
    if (out < in) {
        while (size--) {
            *out++ = *in++;
        }
    } else {
        while (size--) {
            out[size] = in[size];
        }
    }
    return to;
}

void* memset(void* to, int value, size_t size) {
    unsigned char* out = to;
    while (size--) {
        *out++ = (unsigned char)value;
    }
    return to;
}

int memcmp(const void* left, const void* right, size_t size) {
    const unsigned char* a = left;
    const unsigned char* b = right;
    for (size_t i = 0; i < size; ++i) {
        if (a[i] != b[i]) {
            return a[i] - b[i];
        }
    }
    return 0;
}

// --- Input: deterministic, compressible text-like bytes ---

EXPORT void kernel_fill(uint8_t* buffer, size_t size, uint64_t seed) {
    static const char words[][8] = {"android", "native", "bridge", "guest", "host", "arm64",
                                    "x86_64", "kernel", "deflate", "sha256", "zlib", "dynarmic",
                                    "qemu", "uxdroid", "thread", "signal"};
    uint64_t state = seed | 1;
    size_t at = 0;
    while (at < size) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        const char* word = words[state & 15];
        for (size_t i = 0; i < 8 && word[i] != 0 && at < size; ++i) {
            buffer[at++] = (uint8_t)word[i];
        }
        if (at < size) {
            buffer[at++] = (state >> 8) % 7 == 0 ? (uint8_t)(state >> 16) : ' ';
        }
    }
}

// --- SHA-256 (FIPS 180-4) ---

static const uint32_t ROUND[64] = {
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
    0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
    0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
    0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
    0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
    0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
    0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
    0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
    0xc67178f2};

static uint32_t rotate(uint32_t value, int bits) { return (value >> bits) | (value << (32 - bits)); }

static void sha256_block(uint32_t state[8], const uint8_t* block) {
    uint32_t w[64];
    for (int i = 0; i < 16; ++i) {
        w[i] = (uint32_t)block[i * 4] << 24 | (uint32_t)block[i * 4 + 1] << 16 |
               (uint32_t)block[i * 4 + 2] << 8 | block[i * 4 + 3];
    }
    for (int i = 16; i < 64; ++i) {
        uint32_t s0 = rotate(w[i - 15], 7) ^ rotate(w[i - 15], 18) ^ (w[i - 15] >> 3);
        uint32_t s1 = rotate(w[i - 2], 17) ^ rotate(w[i - 2], 19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16] + s0 + w[i - 7] + s1;
    }
    uint32_t a = state[0], b = state[1], c = state[2], d = state[3];
    uint32_t e = state[4], f = state[5], g = state[6], h = state[7];
    for (int i = 0; i < 64; ++i) {
        uint32_t t1 = h + (rotate(e, 6) ^ rotate(e, 11) ^ rotate(e, 25)) + ((e & f) ^ (~e & g)) +
                      ROUND[i] + w[i];
        uint32_t t2 = (rotate(a, 2) ^ rotate(a, 13) ^ rotate(a, 22)) + ((a & b) ^ (a & c) ^ (b & c));
        h = g;
        g = f;
        f = e;
        e = d + t1;
        d = c;
        c = b;
        b = a;
        a = t1 + t2;
    }
    state[0] += a;
    state[1] += b;
    state[2] += c;
    state[3] += d;
    state[4] += e;
    state[5] += f;
    state[6] += g;
    state[7] += h;
}

EXPORT void kernel_sha256(const uint8_t* data, size_t size, uint8_t digest[32]) {
    uint32_t state[8] = {0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
                         0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19};
    size_t whole = size & ~(size_t)63;
    for (size_t at = 0; at < whole; at += 64) {
        sha256_block(state, data + at);
    }
    uint8_t tail[128] = {0};
    size_t rest = size - whole;
    memcpy(tail, data + whole, rest);
    tail[rest] = 0x80;
    size_t tail_size = rest < 56 ? 64 : 128;
    uint64_t bits = (uint64_t)size * 8;
    for (int i = 0; i < 8; ++i) {
        tail[tail_size - 1 - i] = (uint8_t)(bits >> (i * 8));
    }
    for (size_t at = 0; at < tail_size; at += 64) {
        sha256_block(state, tail + at);
    }
    for (int i = 0; i < 8; ++i) {
        digest[i * 4] = (uint8_t)(state[i] >> 24);
        digest[i * 4 + 1] = (uint8_t)(state[i] >> 16);
        digest[i * 4 + 2] = (uint8_t)(state[i] >> 8);
        digest[i * 4 + 3] = (uint8_t)state[i];
    }
}

// --- deflate, with zlib's state in a static arena ---

static unsigned char arena[1 << 20] __attribute__((aligned(16)));
static size_t arena_used;

static voidpf arena_alloc(voidpf opaque, uInt items, uInt size) {
    (void)opaque;
    size_t bytes = ((size_t)items * size + 15) & ~(size_t)15;
    if (arena_used + bytes > sizeof(arena)) {
        return Z_NULL;
    }
    void* block = arena + arena_used;
    arena_used += bytes;
    return block;
}

static void arena_free(voidpf opaque, voidpf address) {
    (void)opaque;
    (void)address;
}

// Compresses `size` bytes at level 6. Returns the compressed size, or 0 on failure.
EXPORT size_t kernel_deflate(const uint8_t* data, size_t size, uint8_t* out, size_t out_size) {
    arena_used = 0;
    z_stream stream;
    memset(&stream, 0, sizeof(stream));
    stream.zalloc = arena_alloc;
    stream.zfree = arena_free;
    if (deflateInit(&stream, 6) != Z_OK) {
        return 0;
    }
    size_t produced = 0;
    size_t consumed = 0;
    const size_t chunk = 1 << 20;
    int status = Z_OK;
    while (status != Z_STREAM_END) {
        size_t input = size - consumed < chunk ? size - consumed : chunk;
        stream.next_in = (Bytef*)(data + consumed);
        stream.avail_in = (uInt)input;
        stream.next_out = out + produced;
        stream.avail_out = (uInt)(out_size - produced < chunk ? out_size - produced : chunk);
        int flush = consumed + input == size ? Z_FINISH : Z_NO_FLUSH;
        size_t before_in = stream.avail_in;
        size_t before_out = stream.avail_out;
        status = deflate(&stream, flush);
        if (status != Z_OK && status != Z_STREAM_END && status != Z_BUF_ERROR) {
            return 0;
        }
        consumed += before_in - stream.avail_in;
        produced += before_out - stream.avail_out;
        if (produced >= out_size && status != Z_STREAM_END) {
            return 0;
        }
    }
    deflateEnd(&stream);
    return produced;
}
