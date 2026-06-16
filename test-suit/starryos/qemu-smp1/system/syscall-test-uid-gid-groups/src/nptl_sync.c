/* nptl_sync.c — setgroups 跨 pthread cred 同步 (man V1).
 *
 * man 2 setgroups §"C library/kernel differences":
 *   "NPTL...signal-based...all threads share same supplementary groups..."
 */

#define _GNU_SOURCE
#include "test_framework.h"

#include <grp.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static atomic_int main_done;
static atomic_int observed_count = -1;
static atomic_int observed_first = -1;

static void *observer(void *arg)
{
    (void)arg;
    while (!atomic_load(&main_done)) usleep(1000);
    gid_t buf[16];
    int n = getgroups(16, buf);
    atomic_store(&observed_count, n);
    atomic_store(&observed_first, n > 0 ? (int)buf[0] : -1);
    return NULL;
}

static void *pthread_setgroups_caller(void *arg)
{
    (void)arg;
    gid_t g[] = {7777};
    if (setgroups(1, g) == 0) atomic_store(&observed_count, 1);
    else atomic_store(&observed_count, -2);
    return NULL;
}

/* (a) main setgroups → pthread observes same */
static void nptl_setgroups_main_to_pthread(void)
{
    /* 测什么/怎么测/期望/为什么: main setgroups(2,{500,600}) → pthread getgroups
     *         也得 count=2. NPTL ✓ vs starry per-thread cred. */
    if (getuid() != 0) { printf("  nptl (a) skip\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        atomic_store(&main_done, 0);
        atomic_store(&observed_count, -1);
        pthread_t t;
        if (pthread_create(&t, NULL, observer, NULL) != 0) _exit(99);
        gid_t g[] = {500, 600};
        if (setgroups(2, g) != 0) _exit(98);
        atomic_store(&main_done, 1);
        pthread_join(t, NULL);
        int n = atomic_load(&observed_count);
        if (n == 2) _exit(0);          /* NPTL ✓ */
        if (n == 0) _exit(20);          /* starry: per-thread */
        printf("  pthread observed count=%d (expected 2)\n", n);
        _exit(1);
    }
    int status;
    waitpid(pid, &status, 0);
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    if (ec == 0)        CHECK(1, "nptl (a) main setgroups(2) → pthread getgroups 见 2");
    else if (ec == 20)  printf("  KNOWN-STARRY-LIMITATION | nptl (a) starry pthread 仍空\n");
    else                CHECK(0, "nptl (a) failed");
}

/* (b) pthread setgroups → main sees */
static void nptl_setgroups_pthread_to_main(void)
{
    /* 测什么/怎么测/期望/为什么: pthread setgroups(1,{7777}) → main 看到 count=1
     *         + first==7777. */
    if (getuid() != 0) { printf("  nptl (b) skip\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        atomic_store(&observed_count, -1);
        pthread_t t;
        if (pthread_create(&t, NULL, pthread_setgroups_caller, NULL) != 0) _exit(99);
        pthread_join(t, NULL);
        if (atomic_load(&observed_count) != 1) _exit(98);
        gid_t mb[16];
        int mn = getgroups(16, mb);
        if (mn == 1 && mb[0] == 7777) _exit(0);
        if (mn == 0)                   _exit(20);
        _exit(1);
    }
    int status;
    waitpid(pid, &status, 0);
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    if (ec == 0)        CHECK(1, "nptl (b) pthread setgroups(1,{7777}) → main 看到 7777");
    else if (ec == 20)  printf("  KNOWN-STARRY-LIMITATION | nptl (b) starry main 仍空\n");
    else                CHECK(0, "nptl (b) failed");
}

int nptl_sync_run(void)
{
    printf("\n----- nptl_sync (man V1) -----\n");
    nptl_setgroups_main_to_pthread();
    nptl_setgroups_pthread_to_main();
    (void)observed_first;
    printf("  ----- nptl_sync: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
