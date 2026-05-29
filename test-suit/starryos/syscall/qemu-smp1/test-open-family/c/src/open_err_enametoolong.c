#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

/* ENAMETOOLONG — pathname 超长。
 *
 * man 2 open §"ENAMETOOLONG" 原文：
 *   "ENAMETOOLONG — pathname was too long."
 *
 * 两种触发：
 *   (1) 单组件（basename）> NAME_MAX (通常 255) → 本模块覆盖（active）
 *   (2) 整路径 > PATH_MAX (4096) → starry 仅查单段，移到
 *       bug-open-pathmax-no-enametoolong（namelen_full_path_too_long
 *       已移除, 见下方注释） */

#define M_DIR OF_MOD("err_namelen")

/* 单组件 > NAME_MAX (255) → ENAMETOOLONG */
static void namelen_single_component_too_long(void)
{
    char path[NAME_MAX + 256];                      /* 充足 buffer 防溢出 */
    int n = snprintf(path, sizeof(path), "%s/", M_DIR);
    if (n < 0 || (size_t)n >= sizeof(path) - 301) {
        CHECK(0, "namelen: prefix too long for buffer");
        return;
    }
    /* 拼一个 300 字符的 basename */
    memset(path + n, 'a', 300);
    path[n + 300] = '\0';

    errno = 0;
    int fd = open(path, O_RDONLY);
    CHECK(fd == -1 && errno == ENAMETOOLONG,                      "namelen: single component 300 chars -> ENAMETOOLONG");
    if (fd >= 0) close(fd);
}

/* namelen_full_path_too_long(): 原为 "整路径 > PATH_MAX → ENAMETOOLONG" 测试用例;
 * 因 starry 与 Linux 行为不一致(starry 仅查单段长度), 已移到 bugfix 复现:
 * test-suit/starryos/bugfix/qemu-smp1/bugfix-smp1/bug-open-pathmax-no-enametoolong/c/src/main.c */

int open_err_enametoolong_run(void)
{
    printf("\n----- open_err_enametoolong -----\n");
    cleanup_tree(M_DIR);
    CHECK(ensure_dir(M_DIR) == 0,                                 "namelen setup: mkdir");
    namelen_single_component_too_long();
    /* namelen_full_path_too_long(): 见上, 已移到 bug-open-pathmax-no-enametoolong */
    cleanup_tree(M_DIR);
    printf("  ----- open_err_enametoolong: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
