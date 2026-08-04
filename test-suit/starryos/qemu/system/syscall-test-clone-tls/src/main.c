/*
 * !test-clone-tls — clone(2) tid flags + set_tid_address(2) + gettid(2) +
 * pthread(CHILD_CLEARTID/SETTLS 真实路径) 穷尽测试
 *
 * ground truth: man 2 clone / set_tid_address / gettid + Linux v7.2 kernel/fork.c。
 * 覆盖 pthread/浏览器线程池命脉:
 *   - 原始 clone: CLONE_CHILD_SETTID(子内存写 tid) / CLONE_PARENT_SETTID(父内存写 tid)
 *   - pthread(内部 CLONE_VM|SIGHAND|THREAD|SETTLS|CHILD_CLEARTID|PARENT_SETTID):
 *     pthread_join 依赖 CHILD_CLEARTID 退出清零 tid + FUTEX_WAKE; __thread 依赖 SETTLS。
 *
 * ★为何 pthread 而非原始 clone 测 CHILD_CLEARTID/SETTLS: musl 的 clone() C wrapper
 * 客户端拒绝这两个 flag(不发 syscall, glibc 才透传), 故这两个 flag 的可移植测试路径
 * 是 pthread(浏览器线程本就走 pthread), 内核对这些 flag 的处理由 pthread 全覆盖。
 *
 * =====================================================================
 * Linux v7.2 源码对齐 (kernel/fork.c)
 * =====================================================================
 *   CHILD_SETTID/PARENT_SETTID 写 tid(fork.c:2138/2763, sched/core.c:5442);
 *   CHILD_CLEARTID 退出清零 ctid + FUTEX_WAKE(mm_users>1, fork.c:1484-1495);
 *   set_tid_address 恒返 TID(fork.c:1812); gettid == task_pid_vnr。
 *   StarryOS: clone.rs do_clone(SETTLS/CHILD_SETTID/PARENT_SETTID/CLEARTID) +
 *   thread.rs set_tid_address/gettid + ops.rs 退出清零+futex wake。
 * =====================================================================
 */

#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
#include "test_framework.h"

#include <stddef.h>
#include <stdint.h>
#include <sched.h>
#include <pthread.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <signal.h>
#include <unistd.h>
#include <string.h>

#ifndef CLONE_PIDFD
#define CLONE_PIDFD 0x00001000
#endif

#define STK (64 * 1024)
#define NTHREAD 8

static long my_gettid(void) { return syscall(SYS_gettid); }
static long my_set_tid_address(int *p) { return syscall(SYS_set_tid_address, p); }

static void alarm_handler(int s)
{
    (void)s;
    const char *m = "\n  FAIL | TIMEOUT | 测试挂死(疑 CHILD_CLEARTID futex wake 缺失)\n"
                    "==== test-clone-tls 汇总: FAIL ====\n";
    ssize_t r = write(2, m, strlen(m));
    (void)r;
    _exit(1);
}

static void *new_stack(void)
{
    void *s = mmap(NULL, STK, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS | MAP_STACK, -1, 0);
    return s == MAP_FAILED ? NULL : (char *)s + STK; /* 栈向下增长, 返回栈顶 */
}

static int child_exit0(void *a) { (void)a; return 0; }

/* ===== A. set_tid_address + gettid ===== */
static int test_sta_gettid(void)
{
    TEST_START("A. set_tid_address + gettid");
    long tid = my_gettid();
    CHECK(tid > 0, "gettid() > 0");
    CHECK(tid == getpid(), "单线程 gettid() == getpid()");

    int loc = 0;
    long r = my_set_tid_address(&loc);
    CHECK(r == tid, "set_tid_address 返回本线程 TID");
    r = my_set_tid_address(NULL);
    CHECK(r == tid, "set_tid_address(NULL) 恒成功返 TID");
    r = my_set_tid_address((int *)(void *)0x1);
    CHECK(r == tid, "set_tid_address(坏指针) 恒成功(只存不解引用)");
    my_set_tid_address(&loc);
    TEST_DONE();
}

/* ===== B. CLONE_CHILD_SETTID (原始 clone) ===== */
static int test_child_settid(void)
{
    TEST_START("B. CLONE_CHILD_SETTID");
    void *stk = new_stack();
    if (!stk) { CHECK(0, "栈"); TEST_DONE(); }
    int ctid = -1;
    pid_t c = clone(child_exit0, stk, CLONE_VM | CLONE_CHILD_SETTID | SIGCHLD, NULL,
                    NULL, NULL, &ctid);
    CHECK(c > 0, "clone(CLONE_VM|CHILD_SETTID) 成功");
    if (c > 0) {
        waitpid(c, NULL, 0);
        CHECK(ctid == c, "ctid 被写为子 TID(== clone 返回值)");
    }
    void *stk2 = new_stack();
    int ctid2 = 0x7777;
    pid_t c2 = clone(child_exit0, stk2, CLONE_VM | SIGCHLD, NULL, NULL, NULL, &ctid2);
    if (c2 > 0) {
        waitpid(c2, NULL, 0);
        CHECK(ctid2 == 0x7777, "无 CHILD_SETTID -> ctid 保持不变");
    }
    TEST_DONE();
}

/* ===== C. CLONE_PARENT_SETTID (原始 clone) ===== */
static int test_parent_settid(void)
{
    TEST_START("C. CLONE_PARENT_SETTID");
    void *stk = new_stack();
    if (!stk) { CHECK(0, "栈"); TEST_DONE(); }
    int ptid = -1;
    pid_t c = clone(child_exit0, stk, CLONE_VM | CLONE_PARENT_SETTID | SIGCHLD, NULL,
                    &ptid, NULL, NULL);
    CHECK(c > 0, "clone(CLONE_VM|PARENT_SETTID) 成功");
    if (c > 0) {
        CHECK(ptid == c, "ptid 在父内存被写为子 TID(clone 返回前)");
        waitpid(c, NULL, 0);
    }
    TEST_DONE();
}

/* ===== D. pthread_create/join = CLONE_CHILD_CLEARTID + FUTEX_WAKE 命脉 ===== */
static volatile int g_ran_count;
static void *join_thr(void *a)
{
    (void)a;
    __sync_fetch_and_add(&g_ran_count, 1);
    return (void *)(uintptr_t)0x1234;
}
static int test_pthread_join(void)
{
    TEST_START("D. pthread_create/join(CLONE_CHILD_CLEARTID+futex wake)");
    /* 单线程 join: pthread_join 内部 futex-wait 线程 tid, 子退出时内核 CHILD_CLEARTID
     * 清零 tid + FUTEX_WAKE 唤醒 join。若内核不清或不 wake -> join 永久阻塞 -> alarm。 */
    pthread_t t;
    g_ran_count = 0;
    int r = pthread_create(&t, NULL, join_thr, NULL);
    CHECK(r == 0, "pthread_create 成功");
    if (r == 0) {
        void *ret = NULL;
        CHECK(pthread_join(t, &ret) == 0, "pthread_join 返回(CHILD_CLEARTID futex wake 生效)");
        CHECK(ret == (void *)(uintptr_t)0x1234, "线程返回值正确回传");
        CHECK(g_ran_count == 1, "线程真跑过");
    }
    /* 多线程压力: 创建 NTHREAD 个并全部 join(线程池模式) */
    pthread_t ts[NTHREAD];
    g_ran_count = 0;
    int created = 0;
    for (int i = 0; i < NTHREAD; i++) {
        if (pthread_create(&ts[i], NULL, join_thr, NULL) == 0) created++;
    }
    CHECK(created == NTHREAD, "批量 pthread_create 全部成功");
    for (int i = 0; i < created; i++) pthread_join(ts[i], NULL);
    CHECK(g_ran_count == created, "批量线程全部 join 完成且都跑过");
    TEST_DONE();
}

/* ===== E. __thread 隔离 = CLONE_SETTLS per-thread TLS ===== */
static __thread int tls_var = 42;
static volatile int child_tls_after_set;
static volatile int child_saw_parent_write;
static void *tls_thr(void *a)
{
    (void)a;
    child_saw_parent_write = tls_var; /* 子线程首次读 __thread: 应是初始 42, 非父设的 7 */
    tls_var = 99;                     /* 子线程改自己的 __thread */
    child_tls_after_set = tls_var;
    return NULL;
}
static int test_pthread_tls(void)
{
    TEST_START("E. __thread 隔离(CLONE_SETTLS per-thread TLS)");
    tls_var = 7; /* 主线程 __thread 设为 7 */
    pthread_t t;
    child_tls_after_set = 0;
    child_saw_parent_write = -1;
    int r = pthread_create(&t, NULL, tls_thr, NULL);
    CHECK(r == 0, "pthread_create(TLS) 成功");
    if (r == 0) {
        pthread_join(t, NULL);
        CHECK(child_saw_parent_write == 42, "子线程 __thread 初值独立(见 42 非父设的 7)");
        CHECK(child_tls_after_set == 99, "子线程改自己 __thread 为 99");
        CHECK(tls_var == 7, "主线程 __thread 不受子影响(SETTLS 隔离)");
    }
    TEST_DONE();
}

/* ===== F. CLONE_PIDFD ∩ CLONE_PARENT_SETTID 互斥(legacy clone) ===== */
static int test_pidfd_parent_settid_einval(void)
{
    TEST_START("F. CLONE_PIDFD|CLONE_PARENT_SETTID legacy -> EINVAL");
    void *stk = new_stack();
    if (!stk) { CHECK(0, "栈"); TEST_DONE(); }
    int ptid = 0;
    errno = 0;
    pid_t c = clone(child_exit0, stk, CLONE_PIDFD | CLONE_PARENT_SETTID | SIGCHLD, NULL,
                    &ptid, NULL, NULL);
    CHECK(c == -1 && errno == EINVAL, "CLONE_PIDFD+PARENT_SETTID legacy -> EINVAL");
    if (c > 0) waitpid(c, NULL, 0);
    TEST_DONE();
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    signal(SIGALRM, alarm_handler);
    alarm(60);
    int fail = 0;
    fail |= test_sta_gettid();
    fail |= test_child_settid();
    fail |= test_parent_settid();
    fail |= test_pthread_join();
    fail |= test_pthread_tls();
    fail |= test_pidfd_parent_settid_einval();
    printf("\n==== test-clone-tls 汇总: %s ====\n", fail ? "FAIL" : "PASS");
    return fail;
}
