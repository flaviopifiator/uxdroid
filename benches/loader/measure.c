// Runs a command repeatedly in fresh processes and reports wall time and peak memory.
// usage: measure <runs> <command> [args...]
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int compare(const void *a, const void *b) {
    double difference = *(const double *)a - *(const double *)b;
    return (difference > 0) - (difference < 0);
}

int main(int argc, char **argv) {
    if (argc < 3) return 2;
    int runs = atoi(argv[1]);
    double *samples = malloc(sizeof(double) * runs);
    long peak_kib = 0;

    for (int i = 0; i < runs; i++) {
        struct timespec start, end;
        clock_gettime(CLOCK_MONOTONIC, &start);
        pid_t child = fork();
        if (child == 0) {
            int null = open("/dev/null", O_WRONLY);
            dup2(null, 1);
            dup2(null, 2);
            execv(argv[2], argv + 2);
            _exit(127);
        }
        int status;
        struct rusage usage;
        wait4(child, &status, 0, &usage);
        clock_gettime(CLOCK_MONOTONIC, &end);
        if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
            printf("failed (status %d)\n", status);
            return 1;
        }
        samples[i] = (end.tv_sec - start.tv_sec) * 1e3 + (end.tv_nsec - start.tv_nsec) / 1e6;
        if (usage.ru_maxrss > peak_kib) peak_kib = usage.ru_maxrss;
    }
    qsort(samples, runs, sizeof(double), compare);
    printf("median %.2f ms  min %.2f ms  p90 %.2f ms  peak %ld KiB\n", samples[runs / 2], samples[0],
           samples[runs * 9 / 10], peak_kib);
    return 0;
}
