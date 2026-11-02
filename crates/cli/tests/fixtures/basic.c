// Built with the Android NDK. Each exported function checks one bionic behavior under glibc.
// Tests call them through `uxdroid load --call`; a check returns 0 on success.
#include <android/api-level.h>
#include <android/log.h>
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <netdb.h>
#include <pthread.h>
#include <semaphore.h>
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

#define CANARY 0x5AFEC0DE5AFEC0DEL

int dep_value(void);
extern int dep_initialized;
__attribute__((weak)) int not_defined_anywhere_weak(void);

static int constructor_ran;
static int dependency_ready_at_constructor;

__attribute__((constructor)) static void on_load(void) {
    constructor_ran = 1;
    dependency_ready_at_constructor = dep_initialized;
}

int answer(void) { return 42; }

int test_constructor(void) { return constructor_ran == 1 ? 0 : 1; }

int test_dependency(void) { return dep_value() == 7 ? 0 : 1; }

int test_init_order(void) { return dependency_ready_at_constructor == 1 ? 0 : 1; }

int test_weak_undefined(void) { return not_defined_anywhere_weak == NULL ? 0 : 1; }

static const char *const relocated_names[] = {"alpha", "beta", "gamma"};

int test_data_relocations(void) {
    return strcmp(relocated_names[2], "gamma") == 0 ? 0 : 1;
}

static char zeroed[1 << 20];

int test_bss(void) {
    for (size_t i = 0; i < sizeof zeroed; i++) {
        if (zeroed[i] != 0) return 1;
    }
    zeroed[0] = 1;
    return 0;
}

int test_errno(void) {
    errno = 0;
    return open("/does/not/exist", O_RDONLY) == -1 && errno == ENOENT ? 0 : 1;
}

int test_stdio(void) {
    char buffer[64];
    snprintf(buffer, sizeof buffer, "%s %d", "printed", 7);
    puts(buffer);
    fprintf(stderr, "%s\n", "to stderr");
    return 0;
}

int test_fortify(void) {
    char buffer[16];
    strcpy(buffer, "fortified");
    size_t length = strlen(buffer);
    char *found = strchr(buffer, 'f');
    char *last = strrchr(buffer, 'i');
    char copy[8];
    strlcpy(copy, buffer, sizeof copy);
    return length == 9 && found == buffer && last == buffer + 6 && strcmp(copy, "fortifi") == 0
               ? 0
               : 1;
}

int test_log(void) {
    __android_log_write(ANDROID_LOG_INFO, "fixture", "plain message");
    __android_log_buf_write(LOG_ID_MAIN, ANDROID_LOG_DEBUG, "fixture", "buffer message");
    // More arguments than registers, plus floats, to exercise every part of the va_list.
    __android_log_print(ANDROID_LOG_WARN, "fixture", "%d %s %.1f %d %d %d %d %d %.1f", 1, "two",
                        3.5, 4, 5, 6, 7, 8, 9.5);
    return 0;
}

static volatile sig_atomic_t signal_count;

static void on_signal(int number) {
    (void)number;
    signal_count++;
}

// A set that bionic's libc would treat as 8 bytes must not write past them.
int test_signal_sets(void) {
    struct {
        sigset_t set;
        long canary;
    } guarded = {.canary = CANARY};
    if (sigemptyset(&guarded.set) != 0) return 1;
    if (sigaddset(&guarded.set, SIGUSR1) != 0) return 2;
    if (sigismember(&guarded.set, SIGUSR1) != 1) return 3;
    if (sigismember(&guarded.set, SIGUSR2) != 0) return 4;
    if (sigdelset(&guarded.set, SIGUSR1) != 0) return 5;
    if (sigfillset(&guarded.set) != 0) return 6;
    if (sigismember(&guarded.set, SIGUSR2) != 1) return 7;
    return guarded.canary == CANARY ? 0 : 8;
}

int test_sigaction(void) {
    struct {
        struct sigaction action;
        long canary;
    } installed = {.canary = CANARY}, previous = {.canary = CANARY};
    installed.action.sa_handler = on_signal;
    sigemptyset(&installed.action.sa_mask);
    if (sigaction(SIGUSR1, &installed.action, &previous.action) != 0) return 1;
    if (raise(SIGUSR1) != 0 || signal_count != 1) return 2;

    struct sigaction current;
    if (sigaction(SIGUSR1, NULL, &current) != 0) return 3;
    if (current.sa_handler != on_signal) return 4;
    return installed.canary == CANARY && previous.canary == CANARY ? 0 : 5;
}

int test_sigmask(void) {
    sigset_t blocked, old, pending;
    sigemptyset(&blocked);
    sigaddset(&blocked, SIGUSR2);
    if (sigprocmask(SIG_BLOCK, &blocked, &old) != 0) return 1;
    if (raise(SIGUSR2) != 0) return 2;
    if (sigpending(&pending) != 0 || sigismember(&pending, SIGUSR2) != 1) return 3;
    int received = 0;
    if (sigwait(&blocked, &received) != 0 || received != SIGUSR2) return 4;
    return pthread_sigmask(SIG_SETMASK, &old, NULL);
}

int test_setjmp(void) {
    static struct {
        jmp_buf buffer;
        long canary;
    } guarded = {.canary = CANARY};
    volatile int visits = 0;
    int value = setjmp(guarded.buffer);
    visits++;
    if (value == 0) longjmp(guarded.buffer, 5);
    return value == 5 && visits == 2 && guarded.canary == CANARY ? 0 : 1;
}

int test_sysconf(void) {
    return sysconf(_SC_PAGESIZE) == 4096 && sysconf(_SC_NPROCESSORS_ONLN) >= 1 &&
                   sysconf(_SC_CLK_TCK) == 100 && sysconf(_SC_OPEN_MAX) >= 1
               ? 0
               : 1;
}

int test_api_level(void) { return android_get_device_api_level() == 35 ? 0 : 1; }

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t changed = PTHREAD_COND_INITIALIZER;
static int shared_value;
static int cleanup_ran;

static void cleanup(void *argument) { *(int *)argument = 1; }

static void *thread_main(void *argument) {
    pthread_cleanup_push(cleanup, &cleanup_ran);
    pthread_mutex_lock(&lock);
    shared_value = *(int *)argument;
    pthread_cond_signal(&changed);
    pthread_mutex_unlock(&lock);
    pthread_exit((void *)123);
    pthread_cleanup_pop(0);
    return NULL;
}

int test_threads(void) {
    int value = 11;
    pthread_t thread;
    pthread_attr_t attributes;
    pthread_attr_init(&attributes);
    if (pthread_create(&thread, &attributes, thread_main, &value) != 0) return 1;
    pthread_mutex_lock(&lock);
    while (shared_value == 0) pthread_cond_wait(&changed, &lock);
    pthread_mutex_unlock(&lock);
    void *result;
    if (pthread_join(thread, &result) != 0 || result != (void *)123) return 2;
    return shared_value == 11 && cleanup_ran == 1 ? 0 : 3;
}

int test_semaphore(void) {
    struct {
        sem_t semaphore;
        long canary;
    } guarded = {.canary = CANARY};
    if (sem_init(&guarded.semaphore, 0, 1) != 0) return 1;
    if (sem_wait(&guarded.semaphore) != 0 || sem_post(&guarded.semaphore) != 0) return 2;
    if (sem_destroy(&guarded.semaphore) != 0) return 3;
    return guarded.canary == CANARY ? 0 : 4;
}

int test_dlopen(void) {
    void *handle = dlopen("libdep.so", RTLD_NOW);
    if (handle == NULL) return 1;
    int (*function)(void) = (int (*)(void))dlsym(handle, "dep_value");
    if (function == NULL || function() != 7) return 2;
    if (dlsym(handle, "no_such_symbol") != NULL || dlerror() == NULL) return 3;
    if (dlerror() != NULL) return 4;
    if (dlsym(RTLD_DEFAULT, "malloc") == NULL) return 5;
    if (dlopen("libno_such_library.so", RTLD_NOW) != NULL || dlerror() == NULL) return 6;
    Dl_info info;
    if (dladdr((void *)function, &info) == 0 || strstr(info.dli_fname, "libdep.so") == NULL)
        return 7;
    return dlclose(handle);
}

int test_getaddrinfo(void) {
    struct addrinfo hints;
    memset(&hints, 0, sizeof hints);
    hints.ai_flags = AI_NUMERICHOST | AI_NUMERICSERV;
    hints.ai_family = AF_INET;
    hints.ai_socktype = SOCK_STREAM;
    struct addrinfo *result = NULL;
    if (getaddrinfo("127.0.0.1", "80", &hints, &result) != 0) return 1;
    if (result->ai_addr == NULL || result->ai_addr->sa_family != AF_INET) return 2;
    if (result->ai_canonname != NULL) return 3;
    freeaddrinfo(result);

    int status = getaddrinfo("not a numeric host", NULL, &hints, &result);
    if (status != EAI_NONAME) return 4;
    return strlen(gai_strerror(status)) > 0 ? 0 : 5;
}

int test_strerror_r(void) {
    char buffer[64];
    int status = strerror_r(ENOENT, buffer, sizeof buffer);
    return status == 0 && strstr(buffer, "No such file") != NULL ? 0 : 1;
}

static int child_handler_ran;

static void on_fork_child(void) { child_handler_ran = 1; }

int test_atfork(void) {
    if (pthread_atfork(NULL, NULL, on_fork_child) != 0) return 1;
    pid_t child = fork();
    if (child == 0) _exit(child_handler_ran == 1 ? 0 : 3);
    int status = 0;
    if (child < 0 || waitpid(child, &status, 0) != child) return 2;
    return WIFEXITED(status) && WEXITSTATUS(status) == 0 ? 0 : 4;
}

int test_getprogname(void) { return strcmp(getprogname(), "uxdroid") == 0 ? 0 : 1; }

static int ifunc_target(void) { return 9; }

static int (*ifunc_resolver(void))(void) { return ifunc_target; }

int ifunc_function(void) __attribute__((ifunc("ifunc_resolver")));

int test_ifunc(void) { return ifunc_function() == 9 ? 0 : 1; }

static __thread int per_thread = 5;

int test_emulated_tls(void) { return per_thread == 5 ? 0 : 1; }

int trap_long_double(void) {
    return strtold("1.5", NULL) > 1.0L ? 0 : 1;
}
