// The arm64 translation spike's test library: one exported function per case the bridge must
// carry across the guest boundary. Built for arm64-v8a with the NDK; each function returns a value
// the test checks.
#include <android/log.h>
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <setjmp.h>
#include <signal.h>
#include <sys/epoll.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <termios.h>
#include <unistd.h>
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

// --- Calls whose arguments differ between arm64 and x86_64 ---

// arm64's struct stat, 128 bytes, filled from the host's.
__attribute__((visibility("default"))) long spike_stat(const char* path, int which) {
    struct stat info;
    memset(&info, 0xAB, sizeof(info));
    int status = -1;
    if (which == 0) {
        status = stat(path, &info);
    } else if (which == 1) {
        status = lstat(path, &info);
    } else if (which == 2) {
        int fd = open(path, O_RDONLY);
        status = fstat(fd, &info);
        close(fd);
    } else {
        status = fstatat(AT_FDCWD, path, &info, 0);
    }
    if (status != 0) {
        return -errno;
    }
    // The size, the file type bits and the link count, each checked by the test.
    return info.st_size * 1000 + (S_ISREG(info.st_mode) ? 100 : 0) + info.st_nlink;
}

// arm64's O_DIRECTORY (040000) is x86_64's O_DIRECT: opening a file with it must fail as a
// directory open does, and F_GETFL must answer in arm64 values.
__attribute__((visibility("default"))) int spike_open_flags(const char* file, const char* directory) {
    int score = 0;
    int fd = open(file, O_RDONLY | O_DIRECTORY);
    if (fd < 0 && errno == ENOTDIR) {
        score += 1;
    }
    fd = openat(AT_FDCWD, directory, O_RDONLY | O_DIRECTORY | O_NOFOLLOW);
    if (fd >= 0) {
        score += 10;
        close(fd);
    }
    fd = open(file, O_RDONLY);
    int flags = fcntl(fd, F_GETFL);
    // The host kernel adds its O_LARGEFILE, which arm64 code must see as its own, not O_NOFOLLOW.
    if ((flags & O_NOFOLLOW) == 0 && (flags & O_ACCMODE) == O_RDONLY) {
        score += 100;
    }
    if (fcntl(fd, F_SETFL, flags | O_NONBLOCK) == 0 && (fcntl(fd, F_GETFL) & O_NONBLOCK)) {
        score += 1000;
    }
    close(fd);
    int pipes[2];
    if (pipe2(pipes, O_CLOEXEC | O_NONBLOCK) == 0) {
        if ((fcntl(pipes[0], F_GETFD) & FD_CLOEXEC) && (fcntl(pipes[0], F_GETFL) & O_NONBLOCK)) {
            score += 10000;
        }
        close(pipes[0]);
        close(pipes[1]);
    }
    return score;
}

// arm64's epoll_event is 16 bytes with data at 8; x86_64's is packed, with data at 4.
__attribute__((visibility("default"))) long spike_epoll(void) {
    int pipes[2];
    if (pipe(pipes) != 0) {
        return -1;
    }
    int poller = epoll_create1(EPOLL_CLOEXEC);
    struct epoll_event event = {};
    event.events = EPOLLIN;
    event.data.u64 = 0x1122334455667788ULL;
    if (epoll_ctl(poller, EPOLL_CTL_ADD, pipes[0], &event) != 0) {
        return -2;
    }
    if (write(pipes[1], "x", 1) != 1) {
        return -3;
    }
    struct epoll_event ready[4];
    memset(ready, 0, sizeof(ready));
    int count = epoll_wait(poller, ready, 4, 1000);
    long result = (count == 1 && ready[0].events == EPOLLIN && ready[0].data.u64 == 0x1122334455667788ULL) ? 1 : -4;
    int available = -1;
    if (ioctl(pipes[0], FIONREAD, &available) == 0 && available == 1) {
        result += 10;
    }
    struct termios terminal;
    if (tcgetattr(pipes[0], &terminal) != 0 && errno == ENOTTY) {
        result += 100;
    }
    // A request outside the converted ones reaches the kernel, which knows FIOASYNC on a pipe.
    int asynchronous = 0;
    if (ioctl(pipes[0], FIOASYNC, &asynchronous) == 0) {
        result += 1000;
    }
    close(poller);
    close(pipes[0]);
    close(pipes[1]);
    return result;
}

static jmp_buf jump;

__attribute__((noinline)) static void jump_back(int value) { longjmp(jump, value); }

// setjmp and longjmp stay in guest code: callee-saved registers and the stack come back.
__attribute__((visibility("default"))) int spike_setjmp(int value) {
    volatile int kept = value * 3;
    int returned = setjmp(jump);
    if (returned == 0) {
        jump_back(value);
        return -1;
    }
    return returned + kept;
}

static volatile sig_atomic_t signalled = 0;

static void on_signal(int number, siginfo_t* info, void*) {
    signalled = number * 10 + (info != nullptr && info->si_signo == number ? 1 : 0);
}

// A guest handler, installed through the host and run when the host delivers the signal.
__attribute__((visibility("default"))) int spike_signal(void) {
    struct sigaction action = {};
    action.sa_sigaction = on_signal;
    action.sa_flags = SA_SIGINFO;
    sigemptyset(&action.sa_mask);
    sigaddset(&action.sa_mask, SIGUSR2);
    struct sigaction previous = {};
    if (sigaction(SIGUSR1, &action, &previous) != 0) {
        return -1;
    }
    raise(SIGUSR1);
    struct sigaction current = {};
    sigaction(SIGUSR1, nullptr, &current);
    int same = current.sa_sigaction == on_signal && sigismember(&current.sa_mask, SIGUSR2) == 1 &&
               (current.sa_flags & SA_SIGINFO) ? 1000 : 0;
    sigaction(SIGUSR1, &previous, nullptr);
    return signalled + same;
}

// The host keeps its own fault handlers, so a guest one is refused, never recorded as installed.
__attribute__((visibility("default"))) int spike_fault_signal(void) {
    struct sigaction action = {};
    action.sa_sigaction = on_signal;
    action.sa_flags = SA_SIGINFO;
    int score = 0;
    if (sigaction(SIGFPE, &action, nullptr) == -1 && errno == EINVAL) {
        score += 1;
    }
    struct sigaction current = {};
    if (sigaction(SIGFPE, nullptr, &current) == 0 && current.sa_sigaction != on_signal) {
        score += 10;
    }
    return score;
}

// arm64's long double is binary128; the host's is x87's.
__attribute__((visibility("default"))) double spike_strtold(const char* text) {
    return static_cast<double>(strtold(text, nullptr) * 2);
}

// More pointers than argument registers: scanf's arguments are placed by its format.
__attribute__((visibility("default"))) int spike_sscanf(const char* text) {
    int a = 0, b = 0, c = 0, d = 0, e = 0, f = 0, g = 0, h = 0, i = 0;
    int read = sscanf(text, "%d %d %d %d %d %d %d %d %d", &a, &b, &c, &d, &e, &f, &g, &h, &i);
    return read * 1000 + a + b + c + d + e + f + g + h + i;
}

// --- The dynamic linker, through the bridge's loader ---

// Each part adds a digit: symbols of a guest library, of the host's libc, a refused lookup with its
// error, dladdr on guest code, and dlclose.
__attribute__((visibility("default"))) int spike_dl(void) {
    int score = 0;
    void* self = dlopen("libspike.so", RTLD_NOW);
    auto add = reinterpret_cast<int (*)(int, int)>(dlsym(self, "spike_add"));
    if (add != nullptr && add(2, 3) == 5) {
        score += 1;
    }
    auto length = reinterpret_cast<size_t (*)(const char*)>(dlsym(RTLD_DEFAULT, "strlen"));
    void* libc = dlopen("libc.so", RTLD_NOW);
    auto pid = reinterpret_cast<pid_t (*)(void)>(dlsym(libc, "getpid"));
    if (length != nullptr && length("four") == 4 && pid != nullptr && pid() == getpid()) {
        score += 10;
    }
    if (dlsym(self, "spike_nonexistent") == nullptr) {
        const char* error = dlerror();
        if (error != nullptr && strstr(error, "spike_nonexistent") != nullptr) {
            score += 100;
        }
    }
    Dl_info info = {};
    if (dladdr(reinterpret_cast<void*>(spike_add), &info) != 0 && info.dli_fname != nullptr &&
        strstr(info.dli_fname, "libspike.so") != nullptr) {
        score += 1000;
    }
    if (dlclose(self) == 0) {
        score += 10000;
    }
    return score;
}

// A GL function found with eglGetProcAddress is a guest stub with the table's signature; one the
// tables do not know is null.
__attribute__((visibility("default"))) int spike_proc_address(void) {
    void* egl = dlopen("libEGL.so", RTLD_NOW);
    auto get = reinterpret_cast<void* (*)(const char*)>(dlsym(egl, "eglGetProcAddress"));
    if (get == nullptr) {
        return -1;
    }
    auto multiply = reinterpret_cast<int (*)(int, int)>(get("glUxdroidMultiply"));
    if (multiply == nullptr) {
        return -2;
    }
    return multiply(6, 7) + (get("glUxdroidUnknown") == nullptr ? 1000 : 0);
}

static int on_fd(int fd, int events, void* data) {
    return fd * 100 + events * 10 + (data == reinterpret_cast<void*>(0x10) ? 1 : 0);
}

// A guest callback handed to an NDK function becomes a host thunk.
__attribute__((visibility("default"))) int spike_looper(void) {
    using AddFd = int (*)(void*, int, int, int, int (*)(int, int, void*), void*);
    auto add = reinterpret_cast<AddFd>(dlsym(dlopen("libandroid.so", RTLD_NOW), "ALooper_addFd"));
    return add == nullptr ? -1 : add(nullptr, 5, 0, 1, on_fd, reinterpret_cast<void*>(0x10));
}

// --- ifuncs, resolved by the guest's own resolver with the guest's HWCAP ---

static int answer_base(void) { return 8; }
static int answer_atomics(void) { return 81; }

// HWCAP_ATOMICS is bit 8: the bridge reports ARMv8.0 with FP and ASIMD only.
__attribute__((visibility("hidden"))) void* resolve_answer(uint64_t hwcap) {
    return (hwcap & (1ULL << 8)) ? reinterpret_cast<void*>(answer_atomics)
                                 : reinterpret_cast<void*>(answer_base);
}

static int hidden_answer(void) __attribute__((ifunc("resolve_answer")));

// Exported, found through its symbol; the hidden one is called through an IRELATIVE slot.
__attribute__((visibility("default"))) int spike_ifunc(void) __attribute__((ifunc("resolve_answer")));

__attribute__((visibility("default"))) int spike_call_ifunc(void) { return hidden_answer() * 2; }

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
