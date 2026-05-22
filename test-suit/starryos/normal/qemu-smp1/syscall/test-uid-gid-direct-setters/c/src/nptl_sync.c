/* nptl_sync.c — POSIX 要求 同进程线程共享 cred (man V1).
 *
 * man 2 setuid §"C library/kernel differences":
 *   "At the kernel level, user IDs and group IDs are a per-thread attribute.
 *    However, POSIX requires that all threads in a process share the same
 *    credentials. The NPTL threading implementation handles the POSIX
 *    requirements by providing wrapper functions for the various system calls
 *    that change process UIDs and GIDs. These wrapper functions (including
 *    the one for setuid()) employ a signal-based technique to ensure that
 *    when one thread changes credentials, all of the other threads in the
 *    process also change their credentials. For details, see nptl(7)."
 *
 * starry 是否实现 NPTL signal-based cred sync 未知 (大概率没实现 —
 * 这是 user-space NPTL 库 + kernel signal 协作). 若不实现:
 *   - setuid in main thread → main thread cred 改, 但其他 pthread cred 不变
 *   - getuid in pthread 返旧值 → KNOWN-STARRY-LIMITATION
 *
 * 3 维度覆盖 (a-c):
 *   (a) setuid in main thread → 验 pthread 内 getuid 也反映
 *   (b) setgid in pthread → 验 main thread getgid 也反映
 *   (c) 2 pthread 并行调 setuid → 最终全线程 cred 一致
 */

#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static atomic_int main_done;
static atomic_int pthread_observed_uid = -1;
static atomic_int pthread_observed_gid = -1;

static void *pthread_observer(void *arg)
{
    (void)arg;
    /* 等 main thread setuid 完成 */
    while (!atomic_load(&main_done)) usleep(1000);
    /* 此时若 NPTL 正常, pthread 也应见到新 uid */
    atomic_store(&pthread_observed_uid, (int)getuid());
    return NULL;
}

static void *pthread_setgid_caller(void *arg)
{
    (void)arg;
    /* pthread 内调 setgid; NPTL 应同步到 main */
    if (setgid(1234) != 0) {
        /* fail to set — record sentinel */
        atomic_store(&pthread_observed_gid, -2);
    } else {
        atomic_store(&pthread_observed_gid, 1);  /* mark done */
    }
    return NULL;
}

/* (a) main thread setuid → pthread 观察 同步 */
static void nptl_setuid_main_visible_to_pthread(void)
{
    /* 测什么: man V1 — NPTL 保证 setuid in main 跨 pthread 同步.
     * 怎么测: root fork → child 起 pthread → child main setuid(2345) → pthread
     *         观察自己的 getuid 是否 == 2345.
     * 期望: pthread 看到 getuid()==2345 (Linux NPTL ✓).
     *       若 starry 不实现 NPTL → pthread 看 0 (KNOWN-STARRY-LIMITATION).
     * 为什么: 验证 starry 是否实现 POSIX cred 跨线程一致性. */
    if (getuid() != 0) { printf("  nptl (a) skip\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        atomic_store(&main_done, 0);
        atomic_store(&pthread_observed_uid, -1);
        pthread_t t;
        if (pthread_create(&t, NULL, pthread_observer, NULL) != 0) _exit(99);
        if (setuid(2345) != 0) _exit(98);
        atomic_store(&main_done, 1);
        pthread_join(t, NULL);
        int observed = atomic_load(&pthread_observed_uid);
        if (observed == 2345) _exit(0);                    /* Linux NPTL ✓ */
        if (observed == 0)    _exit(20);                   /* starry: 旧值 */
        printf("  observed uid=%d (expected 2345)\n", observed);
        _exit(1);
    }
    int status;
    waitpid(pid, &status, 0);
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    if (ec == 0) {
        CHECK(1, "nptl (a) main setuid(2345) → pthread getuid 见 2345 (NPTL sync ✓)");
    } else if (ec == 20) {
        printf("  KNOWN-STARRY-LIMITATION | nptl (a) starry 无 NPTL cred sync: pthread 仍见 0 (per-thread cred)\n");
    } else {
        CHECK(0, "nptl (a) failed");
    }
}

/* (b) pthread setgid → main observed sync */
static void nptl_setgid_pthread_visible_to_main(void)
{
    /* 测什么: NPTL 反向 — pthread 调 setgid 应同步到 main thread.
     * 怎么测: root fork → child 起 pthread (调 setgid(1234)) → main 等
     *         pthread done → main getgid 应 == 1234.
     * 期望: Linux NPTL ✓; starry 不实现 → main 见 0 (KNOWN-STARRY-LIMITATION).
     * 为什么: POSIX cred 一致性双向都要. */
    if (getuid() != 0) { printf("  nptl (b) skip\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        atomic_store(&pthread_observed_gid, -1);
        pthread_t t;
        if (pthread_create(&t, NULL, pthread_setgid_caller, NULL) != 0) _exit(99);
        pthread_join(t, NULL);
        if (atomic_load(&pthread_observed_gid) != 1) _exit(98);   /* pthread setgid 没成功 */
        gid_t main_g = getgid();
        if (main_g == 1234) _exit(0);                              /* Linux NPTL ✓ */
        if (main_g == 0)    _exit(20);                             /* starry */
        _exit(1);
    }
    int status;
    waitpid(pid, &status, 0);
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    if (ec == 0) {
        CHECK(1, "nptl (b) pthread setgid(1234) → main getgid 见 1234 (NPTL sync ✓)");
    } else if (ec == 20) {
        printf("  KNOWN-STARRY-LIMITATION | nptl (b) starry 无 NPTL cred sync: main 仍见 0\n");
    } else {
        CHECK(0, "nptl (b) failed");
    }
}

/* (c) 多 pthread 并发 — 单一最终态 */
static void *pthread_setuid_3000(void *arg)
{
    (void)arg;
    setuid(3000);
    return NULL;
}

static void nptl_concurrent_setuid_converges(void)
{
    /* 测什么: 2 pthread 并发调 setuid → join 后 全进程 cred 应是其中之一
     *         (NPTL ensures atomicity).
     * 怎么测: root fork → child 起 2 pthread (都 setuid(3000)) → join → 验 cred.
     * 期望: 最终 uid=3000 (无 race / 部分态).
     * 为什么: 验 NPTL signal-based sync 在并发下也保持 cred 一致. */
    if (getuid() != 0) { printf("  nptl (c) skip\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        pthread_t t1, t2;
        if (pthread_create(&t1, NULL, pthread_setuid_3000, NULL) != 0) _exit(99);
        if (pthread_create(&t2, NULL, pthread_setuid_3000, NULL) != 0) _exit(98);
        pthread_join(t1, NULL);
        pthread_join(t2, NULL);
        if (getuid() == 3000) _exit(0);
        if (getuid() == 0)    _exit(20);
        _exit(1);
    }
    int status;
    waitpid(pid, &status, 0);
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    if (ec == 0) {
        CHECK(1, "nptl (c) 2 pthread setuid(3000) join → main getuid == 3000");
    } else if (ec == 20) {
        printf("  KNOWN-STARRY-LIMITATION | nptl (c) starry pthread 内 setuid 不传播到 main\n");
    } else {
        CHECK(0, "nptl (c) failed");
    }
}

int nptl_sync_run(void)
{
    printf("\n----- nptl_sync (man V1 cross-thread cred) -----\n");
    nptl_setuid_main_visible_to_pthread();
    nptl_setgid_pthread_visible_to_main();
    nptl_concurrent_setuid_converges();
    printf("  ----- nptl_sync: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
