// The arm64 translation spike's test library: one exported function per case the bridge must
// carry across the guest boundary. Built for arm64-v8a with the NDK; each function returns a value
// the test checks.
#include <android/log.h>
#include <jni.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <stdexcept>

extern "C" {

// Plain arithmetic: the first guest call.
__attribute__((visibility("default"))) int spike_add(int a, int b) { return a + b; }

// Integers and floating point in every argument register, then on the stack.
__attribute__((visibility("default"))) double spike_mix(int a, double b, int64_t c, float d, int e,
                                                        int f, int g, int h, int i, int j,
                                                        double k, double l, double m, double n,
                                                        double o, double p, double q, double r) {
    return a + b + double(c) + d + e + f + g + h + i * 10 + j * 100 + k + l + m + n + o + p +
           q * 1000 + r * 10000;
}

static int compare_ints(const void* left, const void* right) {
    int a = *static_cast<const int*>(left);
    int b = *static_cast<const int*>(right);
    return (a > b) - (a < b);
}

// A host qsort calling a guest comparator. Returns the sorted digits as one number.
__attribute__((visibility("default"))) int spike_qsort(void) {
    int values[] = {5, 3, 9, 1, 7, 2, 8};
    qsort(values, sizeof(values) / sizeof(values[0]), sizeof(values[0]), compare_ints);
    int result = 0;
    for (int value : values) {
        result = result * 10 + value;
    }
    return result;
}

static thread_local int counter = 0;

static void* worker(void* argument) {
    // A fresh thread sees its own counter at zero.
    int seen = counter;
    counter += 5;
    return reinterpret_cast<void*>(static_cast<intptr_t>(*static_cast<int*>(argument) * 2 + seen + counter));
}

// A host thread running a guest start routine; thread_local stays per thread.
__attribute__((visibility("default"))) int spike_thread(int value) {
    counter = 100;
    pthread_t thread;
    if (pthread_create(&thread, nullptr, worker, &value) != 0) {
        return -1;
    }
    void* result = nullptr;
    if (pthread_join(thread, &result) != 0) {
        return -2;
    }
    // The worker's writes did not reach this thread's counter.
    return static_cast<int>(reinterpret_cast<intptr_t>(result)) + counter * 1000;
}

// A C++ exception thrown and caught inside the guest, through its own unwinder.
__attribute__((visibility("default"))) int spike_exception(int value) {
    try {
        if (value > 0) {
            throw std::runtime_error("guest exception");
        }
        return 0;
    } catch (const std::runtime_error& error) {
        return static_cast<int>(strlen(error.what())) + value;
    }
}

// A variadic host call: the format decides where each argument is.
__attribute__((visibility("default"))) int spike_log(void) {
    return __android_log_print(ANDROID_LOG_INFO, "spike", "int %d, string %s, double %.2f, long %ld",
                               42, "guest", 2.5, 1234567890123L);
}

// The same variadic path into a buffer the test can read.
__attribute__((visibility("default"))) int spike_format(char* out, size_t size) {
    return snprintf(out, size, "int %d, string %s, double %.2f, long %ld", 42, "guest", 2.5,
                    1234567890123L);
}

// A system call made directly, as Go, Rust's std and hand-written assembly do: `svc #0` with the
// arm64 number in x8.
static long raw_syscall(long number) {
    register long x8 asm("x8") = number;
    register long x0 asm("x0") = 0;
    asm volatile("svc #0" : "+r"(x0) : "r"(x8) : "memory");
    return x0;
}

__attribute__((visibility("default"))) long spike_raw_getpid(void) { return raw_syscall(172); }

// A number no kernel has: the kernel's answer is -ENOSYS in x0.
__attribute__((visibility("default"))) long spike_raw_unknown(void) { return raw_syscall(4095); }

// --- JNI ---

static jint native_add(JNIEnv*, jclass, jint a, jint b) { return a + b; }

static jdouble native_mix(JNIEnv*, jclass, jint a, jlong b, jdouble c, jfloat d, jint e, jint f,
                          jint g, jint h, jint i, jdouble j) {
    return a + double(b) + c + d + e + f + g + h + i * 10 + j * 100;
}

static jstring native_greet(JNIEnv* env, jclass, jstring name) {
    const char* text = env->GetStringUTFChars(name, nullptr);
    char buffer[64];
    strcpy(buffer, "hello ");
    strncat(buffer, text, sizeof(buffer) - strlen(buffer) - 1);
    env->ReleaseStringUTFChars(name, text);
    return env->NewStringUTF(buffer);
}

static const JNINativeMethod METHODS[] = {
    {"add", "(II)I", reinterpret_cast<void*>(native_add)},
    {"mix", "(IJDFIIIIID)D", reinterpret_cast<void*>(native_mix)},
    {"greet", "(Ljava/lang/String;)Ljava/lang/String;", reinterpret_cast<void*>(native_greet)},
};

__attribute__((visibility("default"))) jint JNI_OnLoad(JavaVM* vm, void*) {
    JNIEnv* env = nullptr;
    if (vm->GetEnv(reinterpret_cast<void**>(&env), JNI_VERSION_1_6) != JNI_OK) {
        return JNI_ERR;
    }
    jclass type = env->FindClass("dev/uxdroid/Spike");
    if (type == nullptr) {
        return JNI_ERR;
    }
    if (env->RegisterNatives(type, METHODS, sizeof(METHODS) / sizeof(METHODS[0])) != JNI_OK) {
        return JNI_ERR;
    }
    return JNI_VERSION_1_6;
}

// Found by name, as ART finds natives nobody registered.
__attribute__((visibility("default"))) jlong Java_dev_uxdroid_Spike_scale(JNIEnv*, jclass, jlong value,
                                                                          jint factor) {
    return value * factor;
}

}  // extern "C"
