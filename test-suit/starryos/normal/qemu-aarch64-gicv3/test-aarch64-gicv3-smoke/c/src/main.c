/*
 * test_aarch64_gicv3_smoke.c
 *
 * 同 test-aarch64-boot-smoke, 但所在测试组用 gic-v3 Cargo feature 构建,
 * 并由 qemu-aarch64.toml 指定 -machine virt,gic-version=3, 用来
 * 验证新引入的 GICv3 后端 + CNTV PPI 27 通用定时器路径在真正
 * GICv3 配置下完成内核启动、把控制权交给用户态.
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

    printf("uname.machine = %s\n", uts.machine);
    printf("userspace alive on GICv3\n");
    printf("DONE: 1 pass, 0 fail\n");
    return 0;
}
