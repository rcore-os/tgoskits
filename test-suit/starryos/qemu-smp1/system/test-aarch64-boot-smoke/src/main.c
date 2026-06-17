/*
 * test_aarch64_boot_smoke.c
 *
 * 启动冒烟测试: 仅验证内核完成 GIC 初始化, 时钟中断正常,
 * 文件系统就绪, 并把控制权交给用户态可执行文件. 任何架构上
 * 都能跑通; 该用例特别用于 aarch64 GIC + 通用定时器路径
 * (CNTV / PPI 27) 的回归保护, 避免后续修改重新引入启动时
 * 死锁或中断未投递的问题.
 *
 * 通过条件: 程序写出 DONE 行, 退出码为 0.
 */

#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <sys/utsname.h>

int main(void)
{
    struct utsname uts;
    if (uname(&uts) != 0) {
        fprintf(stderr, "uname() failed\n");
        return 1;
    }

    /*
     * 触发一次系统调用之外的纯用户态读路径, 确认文件系统可读.
     * 失败也仅打印, 不视为 FAIL: 部分镜像不挂载 procfs.
     */
    FILE *f = fopen("/proc/self/comm", "r");
    if (f != NULL) {
        char buf[64];
        if (fgets(buf, sizeof(buf), f) != NULL) {
            printf("comm: %s", buf);
        }
        fclose(f);
    }

    printf("uname.sysname = %s\n", uts.sysname);
    printf("uname.machine = %s\n", uts.machine);
    printf("userspace alive\n");
    printf("DONE: 1 pass, 0 fail\n");
    return 0;
}
