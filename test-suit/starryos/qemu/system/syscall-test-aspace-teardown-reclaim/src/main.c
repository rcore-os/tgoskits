/*
 * test_aspace_teardown_reclaim.c — 被杀进程的阻塞兄弟线程不得推迟地址空间回收
 * (aspace-teardown-reclaim 回归).
 *
 * 回归背景 (为什么写这个测例):
 *   进程被组杀 (SIGKILL) 时，一个停在裸 WaitQueue 上的兄弟线程 (例如阻塞在空
 *   管道的 read()、或 futex 等待) **不会** 被 zap_thread 的 interrupt() 唤醒
 *   —— 裸 WaitQueue 没有注册 interrupt waker，interrupt() 对它是 no-op。于是
 *   该线程一直挂到异步 GC 才退，推迟了 AddrSpace::clear() 及其匿名页帧回收。
 *   在快速循环杀进程的场景下，被杀子进程的匿名内存 (usages[VirtMem]/AnonPages)
 *   一直不在退出时下降、持续累积 → 饿死内存。
 *
 * 修复 (kernel: zap_thread 对被杀线程调用 `ax_task::wake_task(&task)`):
 *   wake_task 是文档化的 escape hatch，强制解除一个 park 的线程。被杀兄弟因此
 *   返回、观察到 pending exit、同步跑 do_exit → 立即回收帧。
 *
 * 判别设计:
 *   循环 ITERS 次: fork 子进程; 子进程 (a) 起一个线程阻塞在空管道 read() (裸
 *   WaitQueue, 永不返回), (b) 主线程 mmap+触碰一大块匿名区 (ANON_MB) 让退出时
 *   回收可观测, 就绪后经管道通知父进程再 pause(); 父进程等就绪 → SIGKILL →
 *   waitpid 回收 → 下一轮。
 *   未修复: 每个被杀子进程的匿名帧不被同步回收, 循环内累积很快耗尽 RAM →
 *   后续子进程 mmap/触碰 OOM (就绪握手 EOF) 或内核 panic → 测例失败。
 *   修复后: 每次被杀子进程的帧同步回收 → 任一时刻只有当前子进程的 ANON_MB
 *   存活, 循环跑满。读 /proc/meminfo MemFree 前后对比, 断言未坍塌 + 跑满全部轮次。
 *   ITERS×ANON_MB=12×48≈576MB 略超各 arch -m 512M 的可用内存 (~300-410MB), 故未修复
 *   在累积到可用内存时 OOM (x86 约 9 轮 / aarch64 约 7 轮); 修复后峰值仅一个子进程的
 *   48MB, 12 轮宽裕。刻意压到接近 OOM 下限而非远超, 使慢 TCG arch (aarch64, 可用内存
 *   最紧) 在 system 组超时预算内跑完 (原 40×96 在 aarch64 TCG 触碰 3.8GB 超时)。
 *   loongarch64 -m 2G 同样跑满 (回收正确即不累积)。
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

#define ITERS 12
#define ANON_MB 48
#define ANON_BYTES ((size_t)ANON_MB * 1024 * 1024)
#define PAGE 4096UL

/* 子进程本地: 阻塞读线程停在这个 fd 上 (裸 WaitQueue) */
static int g_block_rd = -1;

static void *blocked_reader(void *arg)
{
    (void)arg;
    char c;
    /* 停在裸 WaitQueue: 空管道 read() 阻塞至有数据或 teardown。zap_thread 的
     * interrupt() 对裸 WaitQueue 是 no-op, 故这个兄弟正是 SIGKILL 时必须被
     * wake_task 强制唤醒的线程。 */
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

/* 子进程主体: 永不正常返回 (由父进程 SIGKILL 结束) */
static void child_body(int ready_wr)
{
    int bp[2];
    if (pipe(bp) != 0)
        _exit(2);
    g_block_rd = bp[0];

    pthread_t th;
    if (pthread_create(&th, NULL, blocked_reader, NULL) != 0)
        _exit(3);

    /* 大匿名区并逐页触碰, 让真实帧回填 → 退出时回收 (VirtMem/AnonPages) 可观测 */
    void *p = mmap(NULL, ANON_BYTES, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (p == MAP_FAILED)
        _exit(4);                 /* 未修复: 前面被杀子进程未回收 → 这里 OOM */
    for (size_t off = 0; off < ANON_BYTES; off += PAGE)
        ((volatile unsigned char *)p)[off] = 0x5A;

    /* 通知父进程"已武装", 主线程随后也 park (等被杀) */
    if (write(ready_wr, "R", 1) != 1)
        _exit(5);
    pause();
    _exit(0);                     /* 不可达 */
}

int main(void)
{
    TEST_START("blocked-sibling teardown reclaims anon synchronously "
               "(aspace-teardown-reclaim)");

    long free_before = read_meminfo_kb("MemFree");
    printf("  INFO | MemFree before loop: %ld kB\n", free_before);

    int completed = 0;
    int spawn_fail = 0, ready_fail = 0, reap_fail = 0;

    for (int i = 0; i < ITERS; i++) {
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
            child_body(ready[1]);
            _exit(0);             /* 不可达 */
        }

        /* 父进程 */
        close(ready[1]);
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

        completed++;
    }

    CHECK(spawn_fail == 0, "fork/pipe succeeded every iteration");
    CHECK(ready_fail == 0,
          "every child armed its anon region before kill (no OOM mid-loop)");
    CHECK(reap_fail == 0, "every SIGKILLed child was reaped");
    CHECK(completed == ITERS,
          "completed all kill/reap iterations without exhausting RAM");

    long free_after = read_meminfo_kb("MemFree");
    printf("  INFO | MemFree after loop: %ld kB (delta=%ld kB over %d iters)\n",
           free_after, free_before - free_after, completed);

    /* 被杀子进程的匿名帧必须同步回收, 否则 MemFree 坍塌。修复后每轮仅剩 fork/exit 页表
     * 与页缓存等合法开销 (实测慢 arch aarch64 约 2~3MB/轮, 全程约 30MB), 远低于此预算;
     * 未修复每轮泄漏一个 ANON_MB=48MB → 在 -m 512M 上累积超可用内存直接 OOM (循环提前中断,
     * 到不了这里), 或在内存更宽裕处 delta 远超此预算。100MB 是宽松阈值: 容开销、抓真泄漏。 */
    long leak_budget = 100L * 1024;   /* 100 MB, 宽松阈值 */
    if (free_before > 0 && free_after > 0) {
        CHECK(free_before - free_after < leak_budget,
              "MemFree did not collapse (blocked-sibling anon was reclaimed)");
    } else {
        printf("  INFO | MemFree unavailable; skipping leak-magnitude assertion\n");
    }

    TEST_DONE();
}
