// Built with the Android NDK. Each exported function checks one bionic behavior under glibc.
// Tests call them through `uxdroid load --call`; a check returns 0 on success.
#include <android/api-level.h>
#include <android/log.h>
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <libgen.h>
#include <math.h>
#include <regex.h>
#include <netdb.h>
#include <pthread.h>
#include <semaphore.h>
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#include <wchar.h>

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

// glibc may write more of a thread object than bionic's type holds; nothing past it may change.
int test_thread_objects_keep_their_bionic_size(void) {
    struct {
        pthread_attr_t attributes;
        long canary;
    } attr = {.canary = CANARY};
    if (pthread_attr_init(&attr.attributes) != 0) return 1;
    pthread_attr_destroy(&attr.attributes);
    if (attr.canary != CANARY) return 2;
    if (pthread_getattr_np(pthread_self(), &attr.attributes) != 0) return 3;
    size_t size = 0;
    if (pthread_attr_getstacksize(&attr.attributes, &size) != 0 || size == 0) return 4;
    pthread_attr_destroy(&attr.attributes);
    if (attr.canary != CANARY) return 5;

    struct {
        pthread_mutex_t mutex;
        long canary;
    } guarded = {.canary = CANARY};
    pthread_mutexattr_t recursive;
    pthread_mutexattr_init(&recursive);
    pthread_mutexattr_settype(&recursive, PTHREAD_MUTEX_RECURSIVE);
    for (int round = 0; round < 2; round++) {
        if (pthread_mutex_init(&guarded.mutex, round == 0 ? NULL : &recursive) != 0) return 6;
        if (pthread_mutex_lock(&guarded.mutex) != 0) return 7;
        int again = pthread_mutex_trylock(&guarded.mutex);
        if (round == 0 ? again != EBUSY : again != 0) return 8;
        if (round == 1) pthread_mutex_unlock(&guarded.mutex);
        if (pthread_mutex_unlock(&guarded.mutex) != 0) return 9;
        if (pthread_mutex_destroy(&guarded.mutex) != 0) return 10;
    }
    pthread_mutexattr_destroy(&recursive);
    return guarded.canary == CANARY ? 0 : 11;
}

// bionic's condition variables and semaphores are 4-aligned and glibc's 8-aligned on arm64,
// where glibc's 64-bit atomics on a 4-mod-8 address fault. A queue that puts them after a mutex
// is ordinary code; each is used both 4 mod 8 and 8-aligned, statically and dynamically initialized.
struct queue {
    int count;
    pthread_mutex_t lock;
    pthread_cond_t ready;
    sem_t slots;
};
struct aligned_queue {
    long count;
    pthread_mutex_t lock;
    pthread_cond_t ready;
    sem_t slots;
};
static struct queue static_queue __attribute__((aligned(8))) = {
    0, PTHREAD_MUTEX_INITIALIZER, PTHREAD_COND_INITIALIZER, {0}};
static struct aligned_queue static_aligned __attribute__((aligned(8))) = {
    0, PTHREAD_MUTEX_INITIALIZER, PTHREAD_COND_INITIALIZER, {0}};

struct waiters {
    pthread_mutex_t *lock;
    pthread_cond_t *ready;
    sem_t *slots;
    int *count;
    int woken;
};

static void *wait_for_count(void *argument) {
    struct waiters *w = argument;
    pthread_mutex_lock(w->lock);
    while (*w->count == 0) pthread_cond_wait(w->ready, w->lock);
    w->woken++;
    pthread_mutex_unlock(w->lock);
    sem_post(w->slots);
    return NULL;
}

static int exercise_queue(pthread_mutex_t *lock, pthread_cond_t *ready, sem_t *slots, int *count) {
    struct waiters w = {lock, ready, slots, count, 0};
    *count = 0;
    if (sem_init(slots, 0, 0) != 0) return 1;
    pthread_t threads[3];
    for (int i = 0; i < 3; i++) {
        if (pthread_create(&threads[i], NULL, wait_for_count, &w) != 0) return 2;
    }
    // One signal, then a broadcast for the rest; each waiter posts the semaphore once.
    pthread_mutex_lock(lock);
    *count = 1;
    pthread_cond_signal(ready);
    pthread_cond_broadcast(ready);
    pthread_mutex_unlock(lock);
    for (int i = 0; i < 3; i++) {
        if (sem_wait(slots) != 0) return 3;
    }
    for (int i = 0; i < 3; i++) pthread_join(threads[i], NULL);
    if (w.woken != 3) return 4;
    int value = -1;
    if (sem_getvalue(slots, &value) != 0 || value != 0) return 5;
    if (sem_trywait(slots) != -1 || errno != EAGAIN) return 6;
    struct timespec soon;
    clock_gettime(CLOCK_REALTIME, &soon);
    soon.tv_nsec += 20 * 1000 * 1000;
    if (soon.tv_nsec >= 1000000000) {
        soon.tv_sec++;
        soon.tv_nsec -= 1000000000;
    }
    if (sem_timedwait(slots, &soon) != -1 || errno != ETIMEDOUT) return 7;
    pthread_mutex_lock(lock);
    int timed = pthread_cond_timedwait(ready, lock, &soon);
    pthread_mutex_unlock(lock);
    if (timed != ETIMEDOUT) return 8;
    if (sem_destroy(slots) != 0) return 9;
    return 0;
}

int test_misaligned_conditions_and_semaphores(void) {
    if ((((unsigned long)&static_queue.ready) & 7) != 4 || (((unsigned long)&static_queue.slots) & 7) != 4)
        return 100;
    int status = exercise_queue(&static_queue.lock, &static_queue.ready, &static_queue.slots,
                                &static_queue.count);
    if (status != 0) return 10 + status;
    if (pthread_cond_destroy(&static_queue.ready) != 0) return 20;
    status = exercise_queue(&static_aligned.lock, &static_aligned.ready, &static_aligned.slots,
                            (int *)&static_aligned.count);
    if (status != 0) return 30 + status;
    // Dynamic ones, initialized and destroyed twice at the same misaligned place.
    void *memory = NULL;
    if (posix_memalign(&memory, 8, sizeof(struct queue)) != 0) return 40;
    struct queue *dynamic = memory;
    for (int round = 0; round < 2; round++) {
        pthread_mutex_init(&dynamic->lock, NULL);
        if (pthread_cond_init(&dynamic->ready, NULL) != 0) return 41;
        status = exercise_queue(&dynamic->lock, &dynamic->ready, &dynamic->slots, &dynamic->count);
        if (status != 0) return 50 + status;
        if (pthread_cond_destroy(&dynamic->ready) != 0) return 60;
        pthread_mutex_destroy(&dynamic->lock);
    }
    free(dynamic);
    return 0;
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

int test_setjmp_restores_the_signal_mask(void) {
    static struct {
        jmp_buf buffer;
        long canary;
    } guarded = {.canary = CANARY};
    sigset_t blocked, current;
    sigemptyset(&blocked);
    sigprocmask(SIG_SETMASK, &blocked, NULL);
    if (setjmp(guarded.buffer) == 0) {
        sigaddset(&blocked, SIGUSR1);
        sigprocmask(SIG_SETMASK, &blocked, NULL);
        longjmp(guarded.buffer, 1);
    }
    sigprocmask(SIG_SETMASK, NULL, &current);
    return sigismember(&current, SIGUSR1) == 0 && guarded.canary == CANARY ? 0 : 1;
}

int test_sigsetjmp(void) {
    static struct {
        sigjmp_buf buffer;
        long canary;
    } guarded = {.canary = CANARY};
    sigset_t blocked, current;
    sigemptyset(&blocked);
    sigprocmask(SIG_SETMASK, &blocked, NULL);
    if (sigsetjmp(guarded.buffer, 0) == 0) {
        sigaddset(&blocked, SIGUSR1);
        sigprocmask(SIG_SETMASK, &blocked, NULL);
        siglongjmp(guarded.buffer, 1);
    }
    sigprocmask(SIG_SETMASK, NULL, &current);
    int kept = sigismember(&current, SIGUSR1);
    sigemptyset(&blocked);
    sigprocmask(SIG_SETMASK, &blocked, NULL);
    return kept == 1 && guarded.canary == CANARY ? 0 : 1;
}

int test_path_helpers_leave_their_input_alone(void) {
    const char *literal = "/usr/lib/";
    if (strcmp(basename(literal), "lib") != 0) return 1;
    if (strcmp(dirname(literal), "/usr") != 0) return 2;
    if (strcmp(dirname("file"), ".") != 0 || strcmp(basename("/"), "/") != 0) return 3;
    return strcmp(dirname(NULL), ".") == 0 && strcmp(literal, "/usr/lib/") == 0 ? 0 : 4;
}

int test_getnameinfo(void) {
    struct sockaddr_in address;
    memset(&address, 0, sizeof address);
    address.sin_family = AF_INET;
    address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    address.sin_port = htons(80);
    char host[64], service[16];
    int status = getnameinfo((struct sockaddr *)&address, sizeof address, host, sizeof host,
                             service, sizeof service, NI_NUMERICHOST | NI_NUMERICSERV);
    return status == 0 && strcmp(host, "127.0.0.1") == 0 && strcmp(service, "80") == 0 ? 0 : 1;
}

int test_pathconf(void) {
    return pathconf("/", _PC_NAME_MAX) == 255 && pathconf("/", _PC_PATH_MAX) == 4096 ? 0 : 1;
}

int test_setbuf(void) {
    static struct {
        char buffer[BUFSIZ];
        long canary;
    } guarded = {.canary = CANARY};
    FILE *file = fopen("/dev/null", "w");
    if (file == NULL) return 1;
    setbuf(file, guarded.buffer);
    for (int i = 0; i < 100; i++) fputs("0123456789abcdef0123456789abcdef", file);
    fclose(file);
    return guarded.canary == CANARY ? 0 : 2;
}

int test_fortified_fread(void) {
    char buffer[16];
    FILE *file = fopen("/dev/zero", "r");
    if (file == NULL) return 1;
    size_t count = fread(buffer, 1, sizeof buffer, file);
    fclose(file);
    return count == sizeof buffer ? 0 : 2;
}

int test_printf_family(void) {
    char buffer[32];
    if (sprintf(buffer, "%d %s", 1, "a") != 3) return 1;
    if (snprintf(buffer, sizeof buffer, "%%%d %.1f", 2, 0.5) != 6 || strcmp(buffer, "%2 0.5") != 0)
        return 2;
    int value = 0;
    if (sscanf("41", "%d", &value) != 1 || value != 41) return 3;
    char *allocated = NULL;
    if (asprintf(&allocated, "%s", "heap") != 4 || strcmp(allocated, "heap") != 0) return 4;
    free(allocated);
    return 0;
}

int trap_printf_long_double(void) {
    char buffer[32];
    return snprintf(buffer, sizeof buffer, "%Lf", 1.5L);
}

int trap_unaudited(void) {
    regex_t regex;
    return regcomp(&regex, "a", 0);
}

void sincosl(long double value, long double *sine, long double *cosine);

int trap_long_double_math(void) {
    long double sine, cosine;
    sincosl(1.0L, &sine, &cosine);
    return sine > 0.0L ? 0 : 1;
}

int trap_log_long_double(void) {
    __android_log_print(ANDROID_LOG_INFO, "fixture", "%Lf", 1.5L);
    return 0;
}

int test_integer_with_l_modifier_is_not_long_double(void) {
    char buffer[32];
    return snprintf(buffer, sizeof buffer, "%Ld", 7LL) == 1 && buffer[0] == '7' ? 0 : 1;
}

int test_multibyte_text_works_by_default(void) {
    wchar_t wide = 0;
    mbstate_t state;
    memset(&state, 0, sizeof state);
    return mbrtowc(&wide, "\xc3\xa9", 2, &state) == 2 && wide == 0xe9 ? 0 : 1;
}

int trap_ilogb(void) {
    // Volatile, or the compiler folds the call away.
    volatile double value = 1.0;
    return ilogb(value);
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
