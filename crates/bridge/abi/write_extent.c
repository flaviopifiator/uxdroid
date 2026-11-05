/* How far into an object glibc's functions write, for the types whose glibc size is larger than
 * bionic's on the same CPU (compare.sh). Each call gets a buffer filled with a marker and an
 * object initialized the way an app would have it; the output is the last byte offset written,
 * plus one, beside bionic's size. A function that writes past bionic's size needs a shim.
 * Build with the target's glibc and run it there (under QEMU user from the dev-arm64 image). */
#define _GNU_SOURCE
#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <time.h>

#define SIZE 128
#define MARK 0xa5

static unsigned char buffer[SIZE] __attribute__((aligned(16)));

static void fill(void) { memset(buffer, MARK, SIZE); }

/* Marks the bytes from `from` on, leaving the object's first bytes as they are. */
static void mark_tail(size_t from) { memset(buffer + from, MARK, SIZE - from); }

static size_t extent(void) {
    size_t end = SIZE;
    while (end > 0 && buffer[end - 1] == MARK) end--;
    return end;
}

static void report(const char* name, size_t bionic, int status) {
    printf("%s: writes %zu of bionic's %zu%s (status %d)\n", name, extent(), bionic,
           extent() > bionic ? ", PAST bionic's object" : "", status);
}

static void* idle(void* argument) { return argument; }

int main(void) {
    pthread_attr_t* attr = (pthread_attr_t*)buffer;
    const size_t attr_bionic = 56;
    size_t stack_size;
    void* stack;
    struct sched_param param = {0};

    fill();
    report("pthread_attr_init", attr_bionic, pthread_attr_init(attr));
#define ATTR_CALL(name, call)                    \
    mark_tail(attr_bionic);                      \
    report(name, attr_bionic, call);
    ATTR_CALL("pthread_attr_setdetachstate", pthread_attr_setdetachstate(attr, PTHREAD_CREATE_DETACHED));
    ATTR_CALL("pthread_attr_setguardsize", pthread_attr_setguardsize(attr, 8192));
    ATTR_CALL("pthread_attr_setschedparam", pthread_attr_setschedparam(attr, &param));
    ATTR_CALL("pthread_attr_setschedpolicy", pthread_attr_setschedpolicy(attr, SCHED_OTHER));
    ATTR_CALL("pthread_attr_setstacksize", pthread_attr_setstacksize(attr, 1 << 20));
    static char own_stack[1 << 16] __attribute__((aligned(16)));
    ATTR_CALL("pthread_attr_setstack", pthread_attr_setstack(attr, own_stack, sizeof own_stack));
    ATTR_CALL("pthread_attr_getstack", pthread_attr_getstack(attr, &stack, &stack_size));
    ATTR_CALL("pthread_attr_getstacksize", pthread_attr_getstacksize(attr, &stack_size));
    ATTR_CALL("pthread_attr_destroy", pthread_attr_destroy(attr));
    fill();
    report("pthread_getattr_np", attr_bionic, pthread_getattr_np(pthread_self(), attr));
    mark_tail(attr_bionic);
    pthread_t thread;
    int created = pthread_attr_init(attr);
    mark_tail(attr_bionic);
    created |= pthread_create(&thread, attr, idle, NULL);
    report("pthread_create (attr)", attr_bionic, created);
    pthread_join(thread, NULL);

    pthread_mutex_t* mutex = (pthread_mutex_t*)buffer;
    const size_t mutex_bionic = 40;
    pthread_mutexattr_t recursive;
    pthread_mutexattr_init(&recursive);
    pthread_mutexattr_settype(&recursive, PTHREAD_MUTEX_RECURSIVE);
    fill();
    report("pthread_mutex_init (default)", mutex_bionic, pthread_mutex_init(mutex, NULL));
    fill();
    report("pthread_mutex_init (recursive)", mutex_bionic, pthread_mutex_init(mutex, &recursive));
#define MUTEX_CALL(name, call)                   \
    mark_tail(mutex_bionic);                     \
    report(name, mutex_bionic, call);
    MUTEX_CALL("pthread_mutex_lock", pthread_mutex_lock(mutex));
    MUTEX_CALL("pthread_mutex_trylock", pthread_mutex_trylock(mutex));
    MUTEX_CALL("pthread_mutex_unlock", pthread_mutex_unlock(mutex));
    MUTEX_CALL("pthread_mutex_unlock", pthread_mutex_unlock(mutex));
    struct timespec soon;
    clock_gettime(CLOCK_REALTIME, &soon);
    soon.tv_sec += 1;
    MUTEX_CALL("pthread_mutex_timedlock", pthread_mutex_timedlock(mutex, &soon));
    MUTEX_CALL("pthread_mutex_unlock", pthread_mutex_unlock(mutex));
    MUTEX_CALL("pthread_mutex_destroy", pthread_mutex_destroy(mutex));
    /* bionic's static initializer is all zeros, which glibc's is too. */
    memset(buffer, 0, mutex_bionic);
    MUTEX_CALL("pthread_mutex_lock (static)", pthread_mutex_lock(mutex));
    MUTEX_CALL("pthread_mutex_unlock (static)", pthread_mutex_unlock(mutex));

    pthread_barrierattr_t* barrier_attr = (pthread_barrierattr_t*)buffer;
    const size_t barrier_attr_bionic = 4;
    fill();
    report("pthread_barrierattr_init", barrier_attr_bionic, pthread_barrierattr_init(barrier_attr));
    mark_tail(barrier_attr_bionic);
    report("pthread_barrierattr_destroy", barrier_attr_bionic,
           pthread_barrierattr_destroy(barrier_attr));
    return 0;
}
