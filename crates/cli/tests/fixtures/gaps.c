// bionic symbols real apps import that uxdroid lacked, one group per rule. Each group builds into
// its own library (-DGAP_<GROUP>), so a group still missing fails only its own load. Each test
// returns 0, or the number of the first check that failed.
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <semaphore.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/select.h>
#include <sys/signalfd.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

#ifdef GAP_DATA
#include <fenv.h>

extern const char* const sys_signame[];
extern const char* const sys_siglist[];
extern int optreset;
extern uintptr_t __stack_chk_guard;
extern unsigned int error_message_count;
extern int error_one_per_line;
extern void (*error_print_progname)(void);
// bionic's struct res_sym.
struct gap_res_sym {
    int number;
    const char* name;
    const char* human_name;
};
extern const struct gap_res_sym __p_class_syms[];
extern const struct gap_res_sym __p_type_syms[];
extern const char* _ctype_;
// libstdc++'s std::nothrow, an empty object.
extern const char _ZSt7nothrow;

static int same(const char* text, const char* expected) {
    return text != NULL && strcmp(text, expected) == 0;
}

int test_data(void) {
    if (!same(sys_signame[SIGSEGV], "SEGV") || !same(sys_signame[SIGHUP], "HUP")) return 1;
    if (!same(sys_signame[SIGSYS], "SYS") || sys_signame[0] != NULL || sys_signame[32] != NULL)
        return 2;
    if (!same(sys_siglist[SIGINT], "Interrupt") || !same(sys_siglist[SIGTRAP], "Trap")) return 3;
    if (!same(sys_siglist[SIGCHLD], "Child exited") || sys_siglist[64] != NULL) return 4;
    if (optreset != 0) return 5;
    optreset = 1;
    if (optreset != 1) return 6;
    if (__stack_chk_guard == 0 || (__stack_chk_guard & 0xff) != 0) return 7;
    if (error_message_count != 0 || error_one_per_line != 0 || error_print_progname != NULL)
        return 8;
    if (__p_class_syms[0].number != 1 || !same(__p_class_syms[0].name, "IN")) return 9;
    if (__p_class_syms[7].number != 1 || __p_class_syms[7].name != NULL) return 10;
    if (__p_type_syms[27].number != 28 || !same(__p_type_syms[27].name, "AAAA") ||
        !same(__p_type_syms[27].human_name, "IPv6 address"))
        return 11;
    if (__p_type_syms[68].name != NULL) return 12;
    // The old <ctype.h> table: one entry for EOF, then bits per byte (_U 1, _L 2, _N 4, _X 0x40).
    if (_ctype_[1 + 'A'] != (0x01 | 0x40) || _ctype_[1 + 'z'] != 0x02 || _ctype_[1 + '7'] != 0x04)
        return 13;
    const uint32_t* environment = (const uint32_t*)FE_DFL_ENV;
#if defined(__x86_64__)
    static const uint32_t expected[] = {0xffff037f, 0xffff0000, 0xffffffff, 0, 0, 0, 0xffff0000,
                                        0x1f80};
#else
    static const uint32_t expected[] = {0, 0};
#endif
    if (sizeof(fenv_t) != sizeof(expected) || memcmp(environment, expected, sizeof(expected)) != 0)
        return 14;
    // Through a volatile pointer, or the compiler drops a reference it knows is not null.
    const char* volatile nothrow = &_ZSt7nothrow;
    if (nothrow == NULL) return 15;
    return 0;
}
#endif

#ifdef GAP_FDSAN
#include <android/fdsan.h>

int test_fdsan(void) {
    uint64_t tag = android_fdsan_create_owner_tag(ANDROID_FDSAN_OWNER_TYPE_FILE, 0x1234);
    if (tag != ((1ULL << 56) | 0x1234)) return 1;
    if (android_fdsan_create_owner_tag(ANDROID_FDSAN_OWNER_TYPE_FILE, 0) != 0) return 2;
    if (strcmp(android_fdsan_get_tag_type(tag), "FILE*") != 0) return 3;
    if (android_fdsan_get_tag_value(tag) != 0x1234) return 4;
    if (android_fdsan_get_error_level() != ANDROID_FDSAN_ERROR_LEVEL_DISABLED) return 5;
    if (android_fdsan_set_error_level(ANDROID_FDSAN_ERROR_LEVEL_FATAL) !=
        ANDROID_FDSAN_ERROR_LEVEL_DISABLED)
        return 6;
    int fd = open("/dev/null", O_RDONLY);
    if (fd < 0) return 7;
    android_fdsan_exchange_owner_tag(fd, 0, tag);
    if (android_fdsan_get_owner_tag(fd) != 0) return 8;
    // A wrong tag is not checked: fdsan is off.
    if (android_fdsan_close_with_tag(fd, 99) != 0) return 9;
    if (fcntl(fd, F_GETFD) != -1 || errno != EBADF) return 10;
    return 0;
}
#endif

#ifdef GAP_PROPERTIES
#include <sys/system_properties.h>

static int visited;

static void visit(const prop_info* property, void* cookie) {
    (void)property;
    *(int*)cookie += 1;
}

int test_properties(void) {
    if (__system_property_foreach(visit, &visited) != 0 || visited != 0) return 1;
    if (__system_property_find_nth(0) != NULL) return 2;
    uint32_t serial = __system_property_area_serial();
    if (__system_property_area_serial() != serial) return 3;
    uint32_t changed = 0;
    struct timespec none = {0, 0};
    if (!__system_property_wait(NULL, serial + 1, &changed, &none) || changed != serial) return 4;
    struct timespec brief = {0, 1000000};
    if (__system_property_wait(NULL, serial, &changed, &brief)) return 5;
    if (__system_property_set("debug.uxdroid.gap", "1") != -1) return 6;
    char value[PROP_VALUE_MAX];
    if (__system_property_get("ro.build.version.sdk", value) != 2 || strcmp(value, "35") != 0)
        return 7;
    return 0;
}
#endif

#ifdef GAP_FORTIFY
void* __memchr_chk(const void* text, int character, size_t count, size_t object_size);
void* __memrchr_chk(const void* text, int character, size_t count, size_t object_size);
ssize_t __sendto_chk(int socket, const void* buffer, size_t count, size_t object_size, int flags,
                     const struct sockaddr* address, socklen_t address_size);
int __ppoll_chk(struct pollfd* fds, nfds_t count, const struct timespec* timeout,
                const sigset_t* mask, size_t fds_size);
int __ppoll64_chk(struct pollfd* fds, nfds_t count, const struct timespec* timeout,
                  const sigset64_t* mask, size_t fds_size);

int test_fortify(void) {
    char text[] = "abcabc";
    if (__memchr_chk(text, 'c', 6, sizeof(text)) != text + 2) return 1;
    if (__memrchr_chk(text, 'a', 6, sizeof(text)) != text + 3) return 2;
    int pair[2];
    if (socketpair(AF_UNIX, SOCK_DGRAM, 0, pair) != 0) return 3;
    if (__sendto_chk(pair[0], "hey", 3, 8, 0, NULL, 0) != 3) return 4;
    struct pollfd fds[1] = {{pair[1], POLLIN, 0}};
    struct timespec none = {0, 0};
    if (__ppoll_chk(fds, 1, &none, NULL, sizeof(fds)) != 1 || !(fds[0].revents & POLLIN)) return 5;
    fds[0].revents = 0;
    if (__ppoll64_chk(fds, 1, &none, NULL, sizeof(fds)) != 1) return 6;
    close(pair[0]);
    close(pair[1]);
    return 0;
}

// The checked calls stop the program when the object is smaller than the count.
int test_fortify_overflow(void) {
    char text[4] = "abc";
    volatile size_t count = 8;
    return __memchr_chk(text, 'z', count, sizeof(text)) == NULL ? 0 : 1;
}
#endif

#ifdef GAP_SIGSET64
int test_sigset64(void) {
    sigset64_t blocked;
    sigemptyset64(&blocked);
    sigaddset64(&blocked, SIGUSR1);
    int pipes[2];
    if (pipe(pipes) != 0) return 1;
    if (write(pipes[1], "x", 1) != 1) return 2;
    struct pollfd fds[1] = {{pipes[0], POLLIN, 0}};
    struct timespec none = {0, 0};
    if (ppoll64(fds, 1, &none, &blocked) != 1) return 3;
    fd_set readable;
    FD_ZERO(&readable);
    FD_SET(pipes[0], &readable);
    if (pselect64(pipes[0] + 1, &readable, NULL, NULL, &none, &blocked) != 1) return 4;
    int signals = signalfd64(-1, &blocked, SFD_CLOEXEC);
    if (signals < 0) return 5;
    close(signals);
    int epoll = epoll_create1(EPOLL_CLOEXEC);
    struct epoll_event event = {.events = EPOLLIN, .data = {.u64 = 77}};
    if (epoll < 0 || epoll_ctl(epoll, EPOLL_CTL_ADD, pipes[0], &event) != 0) return 6;
    struct epoll_event ready[2];
    if (epoll_pwait64(epoll, ready, 2, 0, &blocked) != 1 || ready[0].data.u64 != 77) return 7;
    close(epoll);
    close(pipes[0]);
    close(pipes[1]);
    return 0;
}
#endif

#ifdef GAP_MONOTONIC
static struct timespec past(void) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    now.tv_sec -= 1;
    return now;
}

int test_monotonic(void) {
    pthread_mutex_t mutex = PTHREAD_MUTEX_INITIALIZER;
    pthread_cond_t cond = PTHREAD_COND_INITIALIZER;
    struct timespec deadline = past();
    if (pthread_mutex_timedlock_monotonic_np(&mutex, &deadline) != 0) return 1;
    if (pthread_cond_timedwait_monotonic_np(&cond, &mutex, &deadline) != ETIMEDOUT) return 2;
    if (pthread_cond_clockwait(&cond, &mutex, CLOCK_MONOTONIC, &deadline) != ETIMEDOUT) return 3;
    pthread_mutex_unlock(&mutex);
    if (pthread_mutex_clocklock(&mutex, CLOCK_MONOTONIC, &deadline) != 0) return 4;
    pthread_mutex_unlock(&mutex);
    pthread_rwlock_t lock = PTHREAD_RWLOCK_INITIALIZER;
    if (pthread_rwlock_timedrdlock_monotonic_np(&lock, &deadline) != 0) return 5;
    if (pthread_rwlock_timedwrlock_monotonic_np(&lock, &deadline) != ETIMEDOUT) return 6;
    pthread_rwlock_unlock(&lock);
    if (pthread_rwlock_clockwrlock(&lock, CLOCK_MONOTONIC, &deadline) != 0) return 7;
    // A read lock by the thread holding the write lock is refused, as in bionic.
    if (pthread_rwlock_clockrdlock(&lock, CLOCK_MONOTONIC, &deadline) != EDEADLK) return 8;
    pthread_rwlock_unlock(&lock);
    sem_t sem;
    if (sem_init(&sem, 0, 1) != 0) return 9;
    if (sem_timedwait_monotonic_np(&sem, &deadline) != 0) return 10;
    if (sem_timedwait_monotonic_np(&sem, &deadline) != -1 || errno != ETIMEDOUT) return 11;
    if (sem_clockwait(&sem, CLOCK_MONOTONIC, &deadline) != -1 || errno != ETIMEDOUT) return 12;
    sem_destroy(&sem);
    return 0;
}
#endif

#ifdef GAP_MISC
#include <android/api-level.h>
#include <malloc.h>
#include <netdb.h>

int test_misc(void) {
    int* h_error = __get_h_errno();
    if (h_error == NULL) return 1;
    *h_error = HOST_NOT_FOUND;
    if (h_errno != HOST_NOT_FOUND) return 2;
    if (android_get_device_api_level() != 35) return 3;
    if (android_get_application_target_sdk_version() != 35) return 4;
    setprogname("/system/bin/gaps");
    if (strcmp(getprogname(), "gaps") != 0) return 5;
    int pipes[2];
    if (pipe(pipes) != 0) return 6;
    if (write(pipes[1], "one\ntwo", 7) != 7) return 7;
    close(pipes[1]);
    FILE* stream = fdopen(pipes[0], "r");
    if (stream == NULL) return 8;
    size_t length = 0;
    char* line = fgetln(stream, &length);
    if (line == NULL || length != 4 || memcmp(line, "one\n", 4) != 0) return 9;
    line = fgetln(stream, &length);
    if (line == NULL || length != 3 || memcmp(line, "two", 3) != 0) return 10;
    if (fgetln(stream, &length) != NULL || length != 0) return 11;
    fclose(stream);
    FILE* out = fdopen(dup(1), "w");
    if (out == NULL) return 12;
    fputs("never written\n", out);
    if (fpurge(out) != 0) return 13;
    fclose(out);
    free_sized(malloc(16), 16);
    free_aligned_sized(memalign(64, 64), 64, 64);
    return 0;
}
#endif
