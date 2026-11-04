/* The sizes and alignments of the libc types bionic and glibc both define, as symbol sizes.
 * compare.sh compiles this once against bionic and once against glibc for the same CPU and lists
 * every type whose size or alignment differs. Kernel types are included as a check. */
#define _GNU_SOURCE
#include <dirent.h>
#include <fcntl.h>
#include <grp.h>
#include <inttypes.h>
#include <locale.h>
#include <mntent.h>
#include <netdb.h>
#include <poll.h>
#include <pthread.h>
#include <pwd.h>
#include <sched.h>
#include <semaphore.h>
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/epoll.h>
#include <sys/resource.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/statfs.h>
#include <sys/statvfs.h>
#include <sys/time.h>
#include <sys/uio.h>
#include <sys/utsname.h>
#include <termios.h>
#include <time.h>
#include <wchar.h>

#define PROBE(name, type)                                   \
    char size_##name[sizeof(type)];                         \
    char align_##name[_Alignof(type)];

PROBE(pthread_t, pthread_t)
PROBE(pthread_attr_t, pthread_attr_t)
PROBE(pthread_mutex_t, pthread_mutex_t)
PROBE(pthread_mutexattr_t, pthread_mutexattr_t)
PROBE(pthread_cond_t, pthread_cond_t)
PROBE(pthread_condattr_t, pthread_condattr_t)
PROBE(pthread_rwlock_t, pthread_rwlock_t)
PROBE(pthread_rwlockattr_t, pthread_rwlockattr_t)
PROBE(pthread_barrier_t, pthread_barrier_t)
PROBE(pthread_barrierattr_t, pthread_barrierattr_t)
PROBE(pthread_spinlock_t, pthread_spinlock_t)
PROBE(pthread_key_t, pthread_key_t)
PROBE(pthread_once_t, pthread_once_t)
PROBE(sem_t, sem_t)
PROBE(sigset_t, sigset_t)
PROBE(jmp_buf, jmp_buf)
PROBE(sigjmp_buf, sigjmp_buf)
PROBE(fpos_t, fpos_t)
PROBE(mbstate_t, mbstate_t)
PROBE(stat, struct stat)
PROBE(statfs, struct statfs)
PROBE(statvfs, struct statvfs)
PROBE(dirent, struct dirent)
PROBE(passwd, struct passwd)
PROBE(group, struct group)
PROBE(tm, struct tm)
PROBE(timespec, struct timespec)
PROBE(timeval, struct timeval)
PROBE(rlimit, struct rlimit)
PROBE(rusage, struct rusage)
PROBE(sigaction, struct sigaction)
PROBE(siginfo_t, siginfo_t)
PROBE(stack_t, stack_t)
PROBE(utsname, struct utsname)
PROBE(sched_param, struct sched_param)
PROBE(cpu_set_t, cpu_set_t)
PROBE(mntent, struct mntent)
PROBE(addrinfo, struct addrinfo)
PROBE(servent, struct servent)
PROBE(lconv, struct lconv)
PROBE(msghdr, struct msghdr)
PROBE(mmsghdr, struct mmsghdr)
PROBE(cmsghdr, struct cmsghdr)
PROBE(sockaddr_storage, struct sockaddr_storage)
PROBE(pollfd, struct pollfd)
PROBE(epoll_event, struct epoll_event)
PROBE(iovec, struct iovec)
PROBE(flock, struct flock)
PROBE(termios, struct termios)
PROBE(itimerval, struct itimerval)
PROBE(itimerspec, struct itimerspec)
PROBE(fd_set, fd_set)
PROBE(div_t, div_t)
PROBE(ldiv_t, ldiv_t)
PROBE(lldiv_t, lldiv_t)
PROBE(imaxdiv_t, imaxdiv_t)
PROBE(wint_t, wint_t)
PROBE(wchar_t, wchar_t)
PROBE(off_t, off_t)
PROBE(time_t, time_t)
PROBE(clock_t, clock_t)
PROBE(long_double, long double)
PROBE(socklen_t, socklen_t)
PROBE(mode_t, mode_t)
PROBE(nlink_t, nlink_t)
PROBE(blksize_t, blksize_t)
PROBE(ino_t, ino_t)
PROBE(dev_t, dev_t)
