// The translation benchmark's game kernel: the work a game's native code does every frame.
//
//   particles  float physics over an array of particles, which the compiler vectorizes (NEON on arm64)
//   matrices   chains of 4x4 float matrix products, as a scene graph's transforms
//   host_calls calls into the host's libc (strlen, sinf), as a game calls libc and GLES every frame
//   tick       one tiny exported function, which the host calls in a loop, as Java calls a game's
//              JNI methods
//
// Floating point is built with -ffp-contract=off, so neither CPU fuses a multiply and an add and
// every configuration computes the same bits. Each phase returns a checksum of its results.
#include <stddef.h>
#include <stdint.h>

#define EXPORT __attribute__((visibility("default")))

size_t strlen(const char* text);
float sinf(float value);

void* memset(void* to, int value, size_t size) {
    unsigned char* out = to;
    while (size--) {
        *out++ = (unsigned char)value;
    }
    return to;
}

static uint64_t mix(uint64_t hash, uint32_t value) {
    hash ^= value;
    return hash * 0x100000001b3u;
}

static uint32_t bits(float value) {
    union {
        float f;
        uint32_t u;
    } pun = {value};
    return pun.u;
}

// --- particles ---

typedef struct {
    float x[4096], y[4096], z[4096];
    float vx[4096], vy[4096], vz[4096];
} Particles;

static Particles particles;

EXPORT uint64_t game_particles(uint32_t steps) {
    enum { COUNT = 4096 };
    for (int i = 0; i < COUNT; ++i) {
        particles.x[i] = (float)(i % 64);
        particles.y[i] = (float)(i / 64);
        particles.z[i] = 0.0f;
        particles.vx[i] = (float)(i % 7) * 0.25f - 0.75f;
        particles.vy[i] = (float)(i % 5) * 0.5f;
        particles.vz[i] = (float)(i % 3) * 0.125f;
    }
    const float dt = 1.0f / 60.0f;
    for (uint32_t step = 0; step < steps; ++step) {
        for (int i = 0; i < COUNT; ++i) {
            float vx = particles.vx[i] * 0.999f;
            float vy = particles.vy[i] * 0.999f - 9.81f * dt;
            float vz = particles.vz[i] * 0.999f;
            float y = particles.y[i] + vy * dt;
            // Bounce off the floor, losing a fifth of the speed.
            float bounced = y < 0.0f ? -vy * 0.8f : vy;
            particles.x[i] += vx * dt;
            particles.y[i] = y < 0.0f ? -y : y;
            particles.z[i] += vz * dt;
            particles.vx[i] = vx;
            particles.vy[i] = bounced;
            particles.vz[i] = vz;
        }
    }
    uint64_t hash = 0xcbf29ce484222325u;
    for (int i = 0; i < COUNT; ++i) {
        hash = mix(hash, bits(particles.x[i]));
        hash = mix(hash, bits(particles.y[i]));
        hash = mix(hash, bits(particles.z[i]));
    }
    return hash;
}

// --- matrices ---

typedef struct {
    float m[16];
} Mat4;

static void multiply(Mat4* out, const Mat4* a, const Mat4* b) {
    for (int row = 0; row < 4; ++row) {
        for (int column = 0; column < 4; ++column) {
            float sum = 0.0f;
            for (int k = 0; k < 4; ++k) {
                sum += a->m[row * 4 + k] * b->m[k * 4 + column];
            }
            out->m[row * 4 + column] = sum;
        }
    }
}

EXPORT uint64_t game_matrices(uint32_t rounds) {
    enum { NODES = 256 };
    static Mat4 local[NODES], world[NODES];
    for (int node = 0; node < NODES; ++node) {
        for (int i = 0; i < 16; ++i) {
            local[node].m[i] = (i % 5 == 0) ? 1.0f : (float)((node + i) % 9) * 0.001f;
        }
    }
    uint64_t hash = 0xcbf29ce484222325u;
    for (uint32_t round = 0; round < rounds; ++round) {
        world[0] = local[0];
        // Each node's parent is the one before it, as a chain of bones.
        for (int node = 1; node < NODES; ++node) {
            multiply(&world[node], &world[node - 1], &local[node]);
        }
        local[round % NODES].m[3] += 0.001f;
        hash = mix(hash, bits(world[NODES - 1].m[round % 16]));
    }
    return hash;
}

// --- host calls ---

EXPORT uint64_t game_host_calls(uint32_t calls) {
    static const char* const names[] = {"u_projection", "u_model", "a_position", "a_normal",
                                        "u_light", "s_texture", "a_uv", "u_time"};
    uint64_t hash = 0xcbf29ce484222325u;
    float angle = 0.0f;
    for (uint32_t call = 0; call < calls; ++call) {
        hash = mix(hash, (uint32_t)strlen(names[call % 8]));
        angle += 0.01f;
        hash = mix(hash, bits(sinf(angle)));
    }
    return hash;
}

// --- tick ---

EXPORT uint64_t game_tick(uint64_t state) { return state * 6364136223846793005u + 1442695040888963407u; }
