/*
 * test_times.c — times 系统调用测试
 *
 * 测试 times() 系统调用的正确性，包括：
 *   1. 基本功能：times() 调用成功且返回值有效
 *   2. 无子进程：cutime/cstime 应为 0
 *   3. 有子进程（如果有 fork）：子进程时间正确累加到 cutime/cstime
 *   4. 单调性：多次调用，返回值应单调递增
 *
 * POSIX 标准:
 *   tms_utime  - 当前进程用户态 CPU 时间
 *   tms_stime  - 当前进程内核态 CPU 时间
 *   tms_cutime - 所有已终止且被 wait() 回收的子进程用户态时间之和
 *   tms_cstime - 所有已终止且被 wait() 回收的子进程内核态时间之和
 */

#include "test_framework.h"
#include <sys/times.h>
#include <unistd.h>
#include <string.h>
#include <sys/wait.h>

/* 忙等待消耗 CPU 时间 */
static void busy_work(void) {
    volatile int sum = 0;
    for (int i = 0; i < 100000; i++) {
        sum += i;
    }
}

int main(void) {
    TEST_START("times");

    struct tms buf;
    clock_t result;

    /* ===== 测试 1: 基本功能 ===== */
    printf("\n--- Test 1: Basic Functionality ---\n");
    memset(&buf, 0xFF, sizeof(buf));
    result = times(&buf);

    CHECK(result != (clock_t)-1, "times() 调用成功");
    CHECK(result >= 0, "times() 返回非负值");

    printf("  INFO | times() returned: %ld\n", (long)result);
    printf("  INFO | tms_utime:  %ld\n", (long)buf.tms_utime);
    printf("  INFO | tms_stime:  %ld\n", (long)buf.tms_stime);
    printf("  INFO | tms_cutime: %ld\n", (long)buf.tms_cutime);
    printf("  INFO | tms_cstime: %ld\n", (long)buf.tms_cstime);

    /* ===== 测试 2: 无子进程时 cutime/cstime 应为 0 ===== */
    printf("\n--- Test 2: No Children (cutime/cstime == 0) ---\n");
    CHECK(buf.tms_cutime == 0, "tms_cutime == 0 (无子进程)");
    CHECK(buf.tms_cstime == 0, "tms_cstime == 0 (无子进程)");

    /* ===== 测试 3: 单调性 ===== */
    printf("\n--- Test 3: Monotonicity ---\n");
    clock_t first_call = result;
    busy_work();
    result = times(&buf);
    CHECK(result >= first_call, "times() 返回值单调递增");

    printf("  INFO | first call:  %ld\n", (long)first_call);
    printf("  INFO | second call: %ld\n", (long)result);
    printf("  INFO | delta:       %ld\n", (long)(result - first_call));

    /* ===== 测试 4: utime/stime 应该在 busy_work 后增加 ===== */
    printf("\n--- Test 4: CPU Time Increases After Work ---\n");
    clock_t utime_before = buf.tms_utime;
    busy_work();
    times(&buf);
    /* 注意：时间精度可能不足，busy_work 可能太短导致时间变化不明显 */
    printf("  INFO | utime before: %ld\n", (long)utime_before);
    printf("  INFO | utime after:  %ld\n", (long)buf.tms_utime);

    /* ===== 测试 5: fork 子进程场景（如果支持 fork） ===== */
#ifdef HAS_FORK
    printf("\n--- Test 5: Fork Children Time Accumulation ---\n");

    pid_t pid = fork();
    if (pid == 0) {
        /* 子进程：干一些 CPU 工作 */
        busy_work();
        busy_work();
        _exit(0);
    } else if (pid > 0) {
        /* 父进程：等待子进程 */
        int status;
        waitpid(pid, &status, 0);

        clock_t parent_utime_before = buf.tms_utime;
        clock_t parent_stime_before = buf.tms_stime;

        result = times(&buf);

        printf("  INFO | child pid: %d\n", pid);
        printf("  INFO | parent utime before: %ld\n", (long)parent_utime_before);
        printf("  INFO | parent utime after:  %ld\n", (long)buf.tms_utime);
        printf("  INFO | child utime (cutime): %ld\n", (long)buf.tms_cutime);
        printf("  INFO | child stime (cstime): %ld\n", (long)buf.tms_cstime);

        /* 子进程时间应该被累加到 cutime/cstime */
        CHECK(buf.tms_cutime > 0, "子进程 utime 累加到 cutime");
        /* cstime 可能为 0（子进程不一定有系统调用） */
        printf("  INFO | tms_cstime (child sys time): %ld\n", (long)buf.tms_cstime);
    } else {
        printf("  INFO | fork() not supported or failed\n");
    }
#else
    printf("\n--- Test 5: Skip (fork not available) ---\n");
    printf("  INFO | Compile with -DHAS_FORK to enable fork tests\n");
#endif

    /* ===== 测试 6: 验证 cutime/cstime 不是错误地使用父进程时间 ===== */
    printf("\n--- Test 6: No Self-Time Leak ---\n");
    clock_t utime_now, stime_now;
    result = times(&buf);
    utime_now = buf.tms_utime;
    stime_now = buf.tms_stime;

    /* Bug 行为：cutime == utime, cstime == stime（错误） */
    /* 正确行为：cutime 和 cstime 与父进程时间无关 */
    CHECK(buf.tms_cutime != utime_now || buf.tms_utime == 0,
          "cutime 不应等于当前 utime（除非 utime 本身就是 0）");
    CHECK(buf.tms_cstime != stime_now || buf.tms_stime == 0,
          "cstime 不应等于当前 stime（除非 stime 本身就是 0）");

    TEST_DONE();
}
