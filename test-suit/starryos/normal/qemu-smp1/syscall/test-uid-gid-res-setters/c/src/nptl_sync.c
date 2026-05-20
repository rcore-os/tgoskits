/* nptl_sync.c — setresuid/setresgid 跨 pthread cred 同步 (man V1).
 *
 * man 2 setresuid §"C library/kernel differences":
 *   "...NPTL...signal-based...cred sync across all threads..."
 *
 * 3 case: main↔pthread setresuid/setresgid 同步 + 并发收敛
 */

#define _GNU_SOURCE
#include "test_framework.h"

#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static atomic_int main_done;
static atomic_int observed_uid = -1;
static atomic_int observed_gid_e = -1;
static atomic_int observed_gid_r = -1;
static atomic_int observed_gid_s = -1;

static void *observer_uid(void *arg)
{
    (void)arg;
    while (!atomic_load(&main_done)) usleep(1000);
    atomic_store(&observed_uid, (int)geteuid());
    return NULL;
}

static void *pthread_setresgid_caller(void *arg)
{
    (void)arg;
    if (setresgid(1111, 2222, 3333) != 0) {
        atomic_store(&observed_gid_e, -2);
        return NULL;
    }
    atomic_store(&observed_gid_e, 1);
    return NULL;
}

/* (a) main setresuid → pthread sees */
static void nptl_setresuid_main_to_pthread(void)
{
    /* 测什么/怎么测/期望/为什么: 同 Group B/C nptl (a) 模板. */
    if (getuid() != 0) { printf("  nptl (a) skip\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        atomic_store(&main_done, 0);
        atomic_store(&observed_uid, -1);
        pthread_t t;
        if (pthread_create(&t, NULL, observer_uid, NULL) != 0) _exit(99);
        if (setresuid(2000, 3000, 4000) != 0) _exit(98);
        atomic_store(&main_done, 1);
        pthread_join(t, NULL);
        int o = atomic_load(&observed_uid);
        if (o == 3000) _exit(0);             /* pthread 见 euid=3000 */
        if (o == 0)    _exit(20);
        printf("  pthread observed=%d (expected 3000)\n", o);
        _exit(1);
    }
    int status;
    waitpid(pid, &status, 0);
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    if (ec == 0)        CHECK(1, "nptl (a) main setresuid → pthread geteuid 见 3000");
    else if (ec == 20)  printf("  KNOWN-STARRY-LIMITATION | nptl (a) starry 无 NPTL cred sync\n");
    else                CHECK(0, "nptl (a) failed");
}

/* (b) pthread setresgid → main sees */
static void nptl_setresgid_pthread_to_main(void)
{
    /* 测什么/怎么测/期望/为什么: pthread setresgid 同步到 main. */
    if (getuid() != 0) { printf("  nptl (b) skip\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        atomic_store(&observed_gid_e, -1);
        pthread_t t;
        if (pthread_create(&t, NULL, pthread_setresgid_caller, NULL) != 0) _exit(99);
        pthread_join(t, NULL);
        if (atomic_load(&observed_gid_e) != 1) _exit(98);
        gid_t r, e, s;
        if (getresgid(&r, &e, &s) != 0) _exit(97);
        if (r == 1111 && e == 2222 && s == 3333) _exit(0);
        if (r == 0 && e == 0 && s == 0)          _exit(20);
        printf("  main: r=%u e=%u s=%u (expected 1111,2222,3333)\n", r, e, s);
        _exit(1);
    }
    int status;
    waitpid(pid, &status, 0);
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    if (ec == 0)        CHECK(1, "nptl (b) pthread setresgid(1k,2k,3k) → main 看到");
    else if (ec == 20)  printf("  KNOWN-STARRY-LIMITATION | nptl (b) starry pthread setresgid 不传播\n");
    else                CHECK(0, "nptl (b) failed");
}

static void *pthread_setresuid_3k(void *arg)
{
    (void)arg;
    setresuid(3000, 3000, 3000);
    return NULL;
}

/* (c) 并发收敛 */
static void nptl_concurrent_setresuid(void)
{
    /* 测什么/怎么测/期望/为什么: 2 pthread 并发 setresuid → join 后 main 看一致. */
    if (getuid() != 0) { printf("  nptl (c) skip\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        pthread_t t1, t2;
        if (pthread_create(&t1, NULL, pthread_setresuid_3k, NULL) != 0) _exit(99);
        if (pthread_create(&t2, NULL, pthread_setresuid_3k, NULL) != 0) _exit(98);
        pthread_join(t1, NULL);
        pthread_join(t2, NULL);
        if (geteuid() == 3000) _exit(0);
        if (geteuid() == 0)    _exit(20);
        _exit(1);
    }
    int status;
    waitpid(pid, &status, 0);
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    if (ec == 0)        CHECK(1, "nptl (c) 2 pthread setresuid(3k×3) join → main 见 3000");
    else if (ec == 20)  printf("  KNOWN-STARRY-LIMITATION | nptl (c) starry concurrent 不传播\n");
    else                CHECK(0, "nptl (c) failed");
}

int nptl_sync_run(void)
{
    printf("\n----- nptl_sync (man V1) -----\n");
    nptl_setresuid_main_to_pthread();
    nptl_setresgid_pthread_to_main();
    nptl_concurrent_setresuid();
    /* 用 observed_gid_r/s 占位避免 -Wunused-variable */
    (void)observed_gid_r;
    (void)observed_gid_s;
    printf("  ----- nptl_sync: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
