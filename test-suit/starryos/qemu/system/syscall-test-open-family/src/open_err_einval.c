#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

/* EINVAL —— flags 中有非法位 / 配置矛盾。
 *
 * man 2 open §"EINVAL" 原文（4 个 variant）：
 *   "EINVAL — The filesystem does not support the O_DIRECT flag. See
 *    NOTES for more information."
 *   "EINVAL — Invalid value in flags."
 *   "EINVAL — O_TMPFILE was specified in flags, but neither O_WRONLY nor
 *    O_RDWR was specified."
 *   "EINVAL — O_CREAT was specified in flags and the final component
 *    (basename) of the new file's pathname is invalid (e.g., it contains
 *    characters not permitted by the underlying filesystem)."
 *
 * 本模块原本测 O_TMPFILE|O_RDONLY → EINVAL；starry 不识别 O_TMPFILE 当
 * silent 处理，移到 bug-open-tmpfile-no-einval。当前模块作为"setup-only"
 * 占位，CREAT|DIRECTORY 已在 flag_matrix + bug-open-creat-directory-einval
 * 覆盖；basename 非法字符在 open_err_misc.c。 */

#define M_DIR  OF_MOD("err_einval")
#define M_FILE M_DIR "/file"

static void mod_setup(void)
{
    cleanup_tree(M_DIR);
    CHECK(ensure_dir(M_DIR) == 0,                                 "einval setup: mkdir");
    CHECK(write_file(M_FILE, "x", 1, 0644) == 0,                  "einval setup: file");
}

/* einval_creat_with_directory(): 原为 "O_CREAT|O_DIRECTORY → EINVAL" 测试用例;
 * 因 starry 与 Linux 行为不一致, 已移到 bugfix 复现:
 * test-suit/starryos/qemu/system/bugfix-bug-open-creat-directory-einval/c/src/main.c */

/* einval_tmpfile_without_write(): 原为 "O_TMPFILE 不配可写 → EINVAL" 测试用例;
 * 因 starry 与 Linux 行为不一致, 已移到 bugfix 复现:
 * test-suit/starryos/qemu/system/bugfix-bug-open-tmpfile-no-einval/c/src/main.c */

/* codex P1 (PR #1, adopted in part): 主 suite 应有 active assertion.
 * 正常 open 不误报 EINVAL — 反向断言验证错误路径 gate 正确. */
static void einval_normal_open_does_not_false_positive(void)
{
    int fd = open(OF_REGULAR, O_RDONLY);
    CHECK(fd >= 0,                                                 "einval positive: open normal file RDONLY 不 EINVAL");
    if (fd >= 0) close(fd);
}

int open_err_einval_run(void)
{
    printf("\n----- open_err_einval -----\n");
    mod_setup();
    einval_normal_open_does_not_false_positive();
    /* 真 EINVAL 触发场景 (starry 不实现) 已移到 bug-*:
     * - bug-open-tmpfile-no-einval / bug-open-creat-directory-einval
     * - bug-open-rdonly-trunc-einval / bug-open-append-trunc-einval */
    cleanup_tree(M_DIR);
    printf("  ----- open_err_einval: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
