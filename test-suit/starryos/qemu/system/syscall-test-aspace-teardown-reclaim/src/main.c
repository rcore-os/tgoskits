/*
 * test_aspace_teardown_reclaim.c — 被杀进程的退出 continuation 必须完成地址空间退役
 * (aspace-teardown-reclaim 回归).
 *
 * 回归背景 (为什么写这个测例):
 *   进程被组杀 (SIGKILL) 时，阻塞在空管道的兄弟线程会由 PollSet/interrupt
 *   唤醒并重新进入调度器完成 do_exit。最后一个用户 owner 可以在这段内核退出
 *   continuation 尚未完成时把 MM 发布为 Retiring；线程持有的 MmPin 必须继续授权
 *   scheduler activation，直到任务不可再运行、页表 root 已切走。若 activation
 *   错误地只接受 Live，父进程会永久卡在 waitpid，匿名页也无法进入退役回收。
 *
 * 修复:
 *   wake_task 让被杀兄弟观察 pending exit；MmPin 作为类型化证明，允许已开始的
 *   内核 continuation 在 Live/Retiring 状态重新 activation。最后 pin 和 CPU lease
 *   释放后，MM 才进入 Retired，并由可睡眠 reclaimer 清理页表和匿名页。
 *
 * 判别设计:
 *   循环 ITERS 次: fork 子进程; 子进程 (a) 起一个线程阻塞在空管道 read(),
 *   (b) 主线程 mmap+触碰一大块匿名区 (ANON_MB) 让退出时
 *   回收可观测, 就绪后经管道通知父进程再 pause(); 父进程等就绪 → SIGKILL →
 *   waitpid 回收 → 下一轮。
 *   未修复 activation: 第一轮 waitpid 永久阻塞。未修复 frame reclaim: 当轮 reap 后
 *   MemFree 在限定时间内无法恢复到基线预算。修复后每轮都完成 group-exit，最后 pin
 *   和 CPU lease 释放，sleepable reclaimer 归还匿名页。
 *   使用 MAP_POPULATE 在一次 syscall 中批量预留/安装 resident 页，再逐页写验证驻留，
 *   避免慢 TCG 为每个 4KiB 页单独往返用户态 fault trap。4×24MiB 足以让单轮 24MiB
 *   泄漏明显越过 16MiB 噪声预算，同时保持四架构 system 单用例在 120 秒内完成。
 *
 * (这是既有调试 app "mmleak-discriminate" 的洁净室替代, 不复用它; 本测例为
 *  模板风格的自包含 C 用例。)
 */

#define _GNU_SOURCE
#include "test_framework.h"
#include <pthread.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>
#include <signal.h>
#include <string.h>

#define ITERS 4
#define ANON_MB 24
#define ANON_BYTES ((size_t)ANON_MB * 1024 * 1024)
#define PAGE 4096UL
#define RECLAIM_BUDGET_KB (16L * 1024)
#define RECLAIM_POLLS 100
#define RECLAIM_POLL_US 50000

/* 子进程本地: 阻塞读线程停在这个 fd 对应的 PollSet 上。 */
static int g_block_rd = -1;

static void *blocked_reader(void *arg)
{
    (void)arg;
    char c;
    /* 空管道 read() 阻塞至有数据或 teardown；SIGKILL 必须使该线程重新运行并
     * 完成 thread-group exit，而不是让它持有 MmPin 到异步 task GC。 */
    (void)read(g_block_rd, &c, 1);
    return NULL;
}

static long read_meminfo_kb(const char *key)
{
    FILE *fp = fopen("/proc/meminfo", "r");
    if (!fp)
        return -1;
    char line[128];
    size_t klen = strlen(key);
    long val = -1;
    while (fgets(line, sizeof line, fp)) {
        if (strncmp(line, key, klen) == 0 && line[klen] == ':') {
            if (sscanf(line + klen + 1, "%ld", &val) != 1)
                val = -1;
            break;
        }
    }
    fclose(fp);
    return val;
}

static long wait_for_reclaim(long baseline_kb)
{
    long observed = read_meminfo_kb("MemFree");
    if (baseline_kb <= 0 || observed <= 0)
        return observed;
    for (int poll = 0;
         poll < RECLAIM_POLLS && baseline_kb - observed >= RECLAIM_BUDGET_KB;
         poll++) {
        usleep(RECLAIM_POLL_US);
        observed = read_meminfo_kb("MemFree");
        if (observed <= 0)
            break;
    }
    return observed;
}

/* 子进程主体: 永不正常返回 (由父进程 SIGKILL 结束) */
static void child_body(int ready_wr, int iteration)
{
    int bp[2];
    if (pipe(bp) != 0)
        _exit(2);
    g_block_rd = bp[0];

    pthread_t th;
    if (pthread_create(&th, NULL, blocked_reader, NULL) != 0)
        _exit(3);
    printf("  INFO | iter %d/%d child pthread ready\n", iteration + 1, ITERS);
    fflush(stdout);

    /* 大匿名区并逐页触碰, 让真实帧回填 → 退出时回收 (VirtMem/AnonPages) 可观测 */
    void *p = mmap(NULL, ANON_BYTES, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS | MAP_POPULATE, -1, 0);
    if (p == MAP_FAILED)
        _exit(4);                 /* 未修复: 前面被杀子进程未回收 → 这里 OOM */
    for (size_t off = 0; off < ANON_BYTES; off += PAGE)
        ((volatile unsigned char *)p)[off] = 0x5A;
    printf("  INFO | iter %d/%d child touched %d MiB\n",
           iteration + 1, ITERS, ANON_MB);
    fflush(stdout);

    /* 通知父进程"已武装", 主线程随后也 park (等被杀) */
    if (write(ready_wr, "R", 1) != 1)
        _exit(5);
    pause();
    _exit(0);                     /* 不可达 */
}

int main(void)
{
    TEST_START("blocked-sibling teardown reclaims anon after MM quiescence "
               "(aspace-teardown-reclaim)");

    long free_before = read_meminfo_kb("MemFree");
    printf("  INFO | MemFree before loop: %ld kB\n", free_before);

    int completed = 0;
    int spawn_fail = 0, ready_fail = 0, reap_fail = 0, reclaim_fail = 0;

    for (int i = 0; i < ITERS; i++) {
        long iteration_free_before = read_meminfo_kb("MemFree");
        int ready[2];
        if (pipe(ready) != 0) {
            spawn_fail++;
            break;
        }

        pid_t pid = fork();
        if (pid < 0) {
            close(ready[0]);
            close(ready[1]);
            spawn_fail++;
            break;
        }
        if (pid == 0) {
            close(ready[0]);
            child_body(ready[1], i);
            _exit(0);             /* 不可达 */
        }

        /* 父进程 */
        close(ready[1]);
        printf("  INFO | iter %d/%d parent waiting for child %d\n",
               i + 1, ITERS, pid);
        fflush(stdout);
        char c;
        ssize_t r = read(ready[0], &c, 1);   /* 等子进程武装 (线程已 park + 已触碰匿名区) */
        close(ready[0]);
        if (r != 1) {
            /* 子进程在武装前就死了 (它的 mmap+触碰因前面被杀子进程未被回收而 OOM)。
             * 收尸并停止。 */
            ready_fail++;
            int st;
            waitpid(pid, &st, 0);
            break;
        }
        printf("  INFO | iter %d/%d child %d armed; sending SIGKILL\n",
               i + 1, ITERS, pid);
        fflush(stdout);

        if (kill(pid, SIGKILL) != 0) {
            reap_fail++;
            int st;
            waitpid(pid, &st, 0);
            break;
        }
        int st;
        if (waitpid(pid, &st, 0) != pid) {
            reap_fail++;
            break;
        }

        long iteration_free_after = wait_for_reclaim(iteration_free_before);
        printf("  INFO | iter %d/%d child %d reaped; MemFree delta=%ld kB\n",
               i + 1, ITERS, pid,
               iteration_free_before > 0 && iteration_free_after > 0
                   ? iteration_free_before - iteration_free_after
                   : -1);
        fflush(stdout);
        if (iteration_free_before > 0 && iteration_free_after > 0 &&
            iteration_free_before - iteration_free_after >= RECLAIM_BUDGET_KB) {
            reclaim_fail++;
            break;
        }
        completed++;
    }

    CHECK(spawn_fail == 0, "fork/pipe succeeded every iteration");
    CHECK(ready_fail == 0,
          "every child armed its anon region before kill (no OOM mid-loop)");
    CHECK(reap_fail == 0, "every SIGKILLed child was reaped");
    CHECK(reclaim_fail == 0,
          "each retired MM returned its resident anonymous pages after quiescence");
    CHECK(completed == ITERS,
          "completed all kill/reap iterations without exhausting RAM");

    long free_after = read_meminfo_kb("MemFree");
    printf("  INFO | MemFree after loop: %ld kB (delta=%ld kB over %d iters)\n",
           free_after, free_before - free_after, completed);

    /* 每轮断言负责捕获一个完整 ANON_MB 泄漏；总量断言再捕获小额持续漂移。 */
    long leak_budget = 32L * 1024;
    if (free_before > 0 && free_after > 0) {
        CHECK(free_before - free_after < leak_budget,
              "MemFree did not collapse (blocked-sibling anon was reclaimed)");
    } else {
        printf("  INFO | MemFree unavailable; skipping leak-magnitude assertion\n");
    }

    TEST_DONE();
}
