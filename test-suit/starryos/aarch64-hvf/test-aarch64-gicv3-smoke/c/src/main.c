/*
 * test_aarch64_gicv3_smoke.c
 *
 * aarch64 HVF 配置专用冒烟测试: 构建时启用 gic-v3 + cntv-timer
 * 两个正交 feature, qemu-aarch64.toml 强制 -machine virt,gic-version=3
 * -cpu cortex-a76. 程序执行 200ms 的 nanosleep, 该路径必须依赖
 * 通用定时器中断按 IRQ 27 投递到 EL1; 如果 GICv3 redistributor
 * 配置错误或 CNTV PPI 没有 enable, 这次 nanosleep 不会返回, 用例
 * 将超时失败.
 *
 * 通过条件: nanosleep 返回 0, 程序写出 DONE 行, 退出码为 0.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

int main(void)
{
    /*
     * 行缓冲, 让上行 success_regex 在 TCG 慢速路径下也能被 harness
     * 立即看到, 避免块缓冲到进程退出才一次性刷出.
     */
    setvbuf(stdout, NULL, _IOLBF, 0);

    struct timespec req = { .tv_sec = 0, .tv_nsec = 200L * 1000L * 1000L };
    struct timespec rem = { 0 };
    if (nanosleep(&req, &rem) != 0) {
        fprintf(stderr, "nanosleep failed: %s\n", strerror(errno));
        return 1;
    }

    printf("nanosleep returned, timer IRQ delivered\n");
    printf("DONE: 1 pass, 0 fail\n");
    return 0;
}
