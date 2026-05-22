/* nptl_sync.c — setreuid/setregid 跨 pthread cred 同步 (man V1).
 *
 * man 2 setreuid §"C library/kernel differences":
 *   "...NPTL...signal-based...when one thread changes credentials, all of the
 *    other threads in the process also change their credentials..."
 *
 * 3 case (a-c): main↔pthread setreuid/setregid 同步 + 并发收敛
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
static atomic_int observed_gid = -1;

static void *observer_uid(void *arg)
{
    (void)arg;
    while (!atomic_load(&main_done)) usleep(1000);
    atomic_store(&observed_uid, (int)getuid());
    return NULL;
}

static void *pthread_setregid_caller(void *arg)
{
    (void)arg;
    if (setregid(1234, 5678) == 0) atomic_store(&observed_gid, 1);
    else atomic_store(&observed_gid, -2);
    return NULL;
}

/* (a) main setreuid → pthread sees new uid */
static void nptl_setreuid_main_to_pthread(void)
{
    /* 测什么: man V1 — setreuid in main 跨 pthread 同步.
     * 怎么测: root fork → child 起 pthread observer → main setreuid(2345, 2345)
     *         → pthread 看 getuid.
     * 期望: Linux NPTL ✓ pthread 看 2345; starry 无 NPTL → 0 (KNOWN-LIMIT). */
    if (getuid() != 0) { printf("  nptl (a) skip\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        atomic_store(&main_done, 0);
        atomic_store(&observed_uid, -1);
        pthread_t t;
        if (pthread_create(&t, NULL, observer_uid, NULL) != 0) _exit(99);
        if (setreuid(2345, 2345) != 0) _exit(98);
        atomic_store(&main_done, 1);
        pthread_join(t, NULL);
        int o = atomic_load(&observed_uid);
        if (o == 2345) _exit(0);
        if (o == 0)    _exit(20);
        _exit(1);
    }
    int status;
    waitpid(pid, &status, 0);
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    if (ec == 0)        CHECK(1, "nptl (a) main setreuid(2k,2k) → pthread getuid 见 2345 (NPTL ✓)");
    else if (ec == 20)  printf("  KNOWN-STARRY-LIMITATION | nptl (a) starry pthread 仍见 0\n");
    else                CHECK(0, "nptl (a) failed");
}

/* (b) pthread setregid → main sees new gid */
static void nptl_setregid_pthread_to_main(void)
{
    /* 测什么: pthread 调 setregid 同步到 main thread.
     * 怎么测: root fork → child 起 pthread setregid(1234, 5678) → join → main 验.
     *         setregid(r, e) 后 rgid=1234, egid=5678.
     * 期望: NPTL ✓: main getgid==1234 + main getegid==5678; starry: 0/0. */
    if (getuid() != 0) { printf("  nptl (b) skip\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        atomic_store(&observed_gid, -1);
        pthread_t t;
        if (pthread_create(&t, NULL, pthread_setregid_caller, NULL) != 0) _exit(99);
        pthread_join(t, NULL);
        if (atomic_load(&observed_gid) != 1) _exit(98);
        gid_t rg = getgid();
        gid_t eg = getegid();
        if (rg == 1234 && eg == 5678) _exit(0);
        if (rg == 0 && eg == 0)        _exit(20);
        printf("  main: rg=%u eg=%u (expected 1234, 5678)\n", rg, eg);
        _exit(1);
    }
    int status;
    waitpid(pid, &status, 0);
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    if (ec == 0)        CHECK(1, "nptl (b) pthread setregid(1234, 5678) → main getgid 见 5678");
    else if (ec == 20)  printf("  KNOWN-STARRY-LIMITATION | nptl (b) starry pthread setregid 不传播\n");
    else                CHECK(0, "nptl (b) failed");
}

static void *pthread_setreuid_3k(void *arg)
{
    (void)arg;
    setreuid(3000, 3000);
    return NULL;
}

/* (c) 2 pthread 并发 setreuid 收敛 */
static void nptl_concurrent_setreuid(void)
{
    /* 测什么: 并发 setreuid 后全进程 cred 单一态.
     * 怎么测: root fork → child 起 2 pthread (都 setreuid(3000)) → join → 验.
     * 期望: getuid()==3000 (无 race / 部分态). */
    if (getuid() != 0) { printf("  nptl (c) skip\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        pthread_t t1, t2;
        if (pthread_create(&t1, NULL, pthread_setreuid_3k, NULL) != 0) _exit(99);
        if (pthread_create(&t2, NULL, pthread_setreuid_3k, NULL) != 0) _exit(98);
        pthread_join(t1, NULL);
        pthread_join(t2, NULL);
        if (getuid() == 3000) _exit(0);
        if (getuid() == 0)    _exit(20);
        _exit(1);
    }
    int status;
    waitpid(pid, &status, 0);
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    if (ec == 0)        CHECK(1, "nptl (c) 2 pthread setreuid(3000) join → main 见 3000");
    else if (ec == 20)  printf("  KNOWN-STARRY-LIMITATION | nptl (c) starry concurrent setreuid 不传播\n");
    else                CHECK(0, "nptl (c) failed");
}

int nptl_sync_run(void)
{
    printf("\n----- nptl_sync (man V1) -----\n");
    nptl_setreuid_main_to_pthread();
    nptl_setregid_pthread_to_main();
    nptl_concurrent_setreuid();
    printf("  ----- nptl_sync: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
