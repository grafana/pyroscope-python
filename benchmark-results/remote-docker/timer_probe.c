#define _GNU_SOURCE
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

static volatile sig_atomic_t hits;
static volatile sig_atomic_t overruns;
static void handler(int s, siginfo_t *info, void *ctx) {
    (void)s; (void)ctx; hits++;
    if (info->si_code == SI_TIMER) overruns += info->si_overrun;
}
static double clock_s(clockid_t clock) {
    struct timespec t;
    if (clock_gettime(clock, &t)) { perror("clock_gettime"); exit(1); }
    return t.tv_sec + t.tv_nsec * 1e-9;
}
int main(int argc, char **argv) {
    int kernel = argc > 1 && !strcmp(argv[1], "syscall");
    int ctimer = argc > 2 && !strcmp(argv[2], "ctimer");
    struct sigaction sa = {.sa_sigaction = handler, .sa_flags = SA_RESTART | SA_SIGINFO};
    sigemptyset(&sa.sa_mask);
    if (sigaction(SIGPROF, &sa, NULL)) return 1;
    struct itimerval it = {.it_interval = {.tv_usec=10000}, .it_value = {.tv_usec=10000}};
    struct rusage before, after;
    getrusage(RUSAGE_SELF, &before);
    double cpu = clock_s(CLOCK_PROCESS_CPUTIME_ID), wall = clock_s(CLOCK_MONOTONIC);
    timer_t timer;
    if (ctimer) {
        struct sigevent sev = {.sigev_notify=SIGEV_SIGNAL, .sigev_signo=SIGPROF};
        struct itimerspec spec = {.it_interval={.tv_nsec=10000000}, .it_value={.tv_nsec=10000000}};
        if (timer_create(CLOCK_THREAD_CPUTIME_ID, &sev, &timer) || timer_settime(timer, 0, &spec, NULL)) return 1;
    } else if (setitimer(ITIMER_PROF, &it, NULL)) return 1;
    volatile unsigned long x = 1;
    while (clock_s(CLOCK_MONOTONIC) - wall < 5) {
        for (int i=0; i<10000; i++) {
            if (kernel) x += syscall(SYS_getppid);
            else x = x * 1664525 + 1013904223;
        }
    }
    it = (struct itimerval){0};
    if (ctimer) timer_delete(timer);
    else setitimer(ITIMER_PROF, &it, NULL);
    cpu = clock_s(CLOCK_PROCESS_CPUTIME_ID) - cpu;
    getrusage(RUSAGE_SELF, &after);
    double user = after.ru_utime.tv_sec-before.ru_utime.tv_sec + (after.ru_utime.tv_usec-before.ru_utime.tv_usec)*1e-6;
    double sys = after.ru_stime.tv_sec-before.ru_stime.tv_sec + (after.ru_stime.tv_usec-before.ru_stime.tv_usec)*1e-6;
    printf("timer=%s mode=%s cpu=%.6f user=%.6f system=%.6f signals=%d overruns=%d weighted=%.6f ratio=%.4f\n", ctimer?"ctimer":"itimer",kernel?"syscall":"user",cpu,user,sys,hits,overruns,hits*.01,hits*.01/cpu);
}
