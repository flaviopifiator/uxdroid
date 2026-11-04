// The arm64 translation spike's test library: one exported function per case the bridge must
// carry across the guest boundary. Built for arm64-v8a with the NDK; each function returns a value
// the test checks.
#include <android/log.h>
#include <jni.h>
#include <pthread.h>
#include <stdarg.h>
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

// --- va_list: the guest's own, read by the bridge ---

static int format_v(char* out, size_t size, const char* format, ...) {
    va_list arguments;
    va_start(arguments, format);
    int written = vsnprintf(out, size, format, arguments);
    va_end(arguments);
    return written;
}

// Nine integers and ten doubles after three fixed arguments: the va_list holds some of each in its
// register save areas and the rest, interleaved, on the stack.
__attribute__((visibility("default"))) int spike_vformat(char* out, size_t size) {
    return format_v(out, size, "%d %d %d %d %d %d %d %d %ld|%.1f %.1f %.1f %.1f %.1f %.1f %.1f %.1f %.1f %.1f|%s",
                    1, 2, 3, 4, 5, 6, 7, 8, 9000000000L, 0.5, 1.5, 2.5, 3.5, 4.5, 5.5, 6.5, 7.5, 8.5,
                    9.5, "end");
}

static char* allocate_v(const char* format, ...) {
    va_list arguments;
    va_start(arguments, format);
    char* text = nullptr;
    if (vasprintf(&text, format, arguments) < 0) {
        text = nullptr;
    }
    va_end(arguments);
    return text;
}

// A string the host frees with its own free(), which is the guest's.
__attribute__((visibility("default"))) char* spike_vasprintf(void) {
    return allocate_v("%s-%d-%.3f", "va", 7, 0.125);
}

static int log_v(const char* format, ...) {
    va_list arguments;
    va_start(arguments, format);
    int written = __android_log_vprint(ANDROID_LOG_WARN, "spike", format, arguments);
    va_end(arguments);
    return written;
}

__attribute__((visibility("default"))) int spike_vlog(void) {
    return log_v("vprint %s %d %.1f", "carried", 64, 0.5);
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

static jint call_int_v(JNIEnv* env, jobject self, jmethodID method, ...) {
    va_list arguments;
    va_start(arguments, method);
    jint result = env->CallIntMethodV(self, method, arguments);
    va_end(arguments);
    return result;
}

// Every way to call Java from native code: variadic, va_list and jvalue array, on instance, static
// and nonvirtual methods and a constructor. Narrow and float arguments are promoted in the
// variadic forms, and the bridge narrows them again by the method's descriptor.
__attribute__((visibility("default"))) jdouble Java_dev_uxdroid_Spike_jniCalls(JNIEnv* env, jobject self) {
    jclass type = env->FindClass("dev/uxdroid/Spike");
    jmethodID sum = env->GetMethodID(type, "sum", "(IJDFZBCS)D");
    jmethodID twice = env->GetMethodID(type, "twice", "(I)I");
    jmethodID triple = env->GetStaticMethodID(type, "triple", "(J)J");
    jmethodID make = env->GetMethodID(type, "<init>", "(I)V");
    jdouble total = env->CallDoubleMethod(self, sum, 1, jlong(2), 0.5, 0.25f, JNI_TRUE, jbyte(-3),
                                          jchar(65), jshort(-7));
    total += call_int_v(env, self, twice, 21);
    total += double(env->CallStaticLongMethod(type, triple, jlong(14)));
    jvalue four[1];
    four[0].i = 4;
    total += env->CallStaticIntMethodA(type, twice, four);
    total += env->CallNonvirtualIntMethod(self, type, twice, 5);
    jobject made = env->NewObject(type, make, 9);
    total += double(reinterpret_cast<intptr_t>(made));
    return total;
}

// Found by name, as ART finds natives nobody registered.
__attribute__((visibility("default"))) jlong Java_dev_uxdroid_Spike_scale(JNIEnv*, jclass, jlong value,
                                                                          jint factor) {
    return value * factor;
}

}  // extern "C"
