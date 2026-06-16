#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

/* ETXTBSY: open running executable for write — Linux 拒绝。
 *
 * man 2 open §"ETXTBSY" 原文：
 *   "ETXTBSY — pathname refers to an executable image which is currently
 *    being executed and write access was requested."
 *
 * 触发条件：
 *   - 目标 fs 上的某进程正在 execve() 运行该 binary（kernel deny_write_access）
 *   - 当前进程 open(file, O_WRONLY) 或 O_RDWR 或 O_TRUNC（任何"写访问"意图）
 *
 * 测试方式：
 *   测例本身就是被 execve() 加载运行的 binary —— 读 /proc/self/exe 拿到当前
 *   binary 路径，再 open 它请求写访问。Linux 行为：-1 ETXTBSY（errno 26）。
 *
 * 路径变体（self-contained 全覆盖）：
 *   (a) open(/proc/self/exe, O_WRONLY) — procfs 软链
 *   (b) open(real_path, O_WRONLY)       — 解析后的真实路径
 *   (c) open(real_path, O_RDWR)         — 写访问的另一形式
 *   (d) open(real_path, O_RDONLY|O_TRUNC) — TRUNC 也算写
 *   (e) open(real_path, O_RDONLY)       — 纯读应该 OK（基线对照）
 *
 * 已知差异：starry 可能不实现 deny_write_access 跟踪 → 不返 ETXTBSY → 走
 * bug-open-etxtbsy 复现路径。本模块对 host 做 hard assert，对 starry 主体
 * 用 should_skip 灰名单挡（若 ETXTBSY 在 starry 失败，转入 bug-* 单独测）。
 */

#define M_DIR  OF_MOD("err_etxtbsy")

/* 读取 /proc/self/exe 返回真实路径；返回 0 表示成功，<0 表示失败 */
static int read_self_exe_path(char *buf, size_t bufsz)
{
    ssize_t n = readlink("/proc/self/exe", buf, bufsz - 1);
    if (n <= 0) return -1;
    buf[n] = '\0';
    return 0;
}

/* (a) /proc/self/exe + O_WRONLY → ETXTBSY */
static void etxtbsy_procself_wronly(void)
{
    errno = 0;
    int fd = open("/proc/self/exe", O_WRONLY);
    int ok = (fd == -1 && errno == ETXTBSY);
    CHECK_OR_BUG(ok, "bug-open-etxtbsy-not-implemented", "ETXTBSY (a) /proc/self/exe O_WRONLY -> -1 ETXTBSY");
    if (fd >= 0) close(fd);
}

/* (b) 真实路径 + O_WRONLY → ETXTBSY */
static void etxtbsy_realpath_wronly(void)
{
    char path[512];
    if (read_self_exe_path(path, sizeof(path)) != 0) {
        CHECK(0, "ETXTBSY (b) skip: readlink /proc/self/exe failed");
        return;
    }
    errno = 0;
    int fd = open(path, O_WRONLY);
    int ok = (fd == -1 && errno == ETXTBSY);
    CHECK_OR_BUG(ok, "bug-open-etxtbsy-not-implemented", "ETXTBSY (b) real_path O_WRONLY -> -1 ETXTBSY");
    if (fd >= 0) close(fd);
}

/* (c) 真实路径 + O_RDWR → ETXTBSY（写访问的另一形式）*/
static void etxtbsy_realpath_rdwr(void)
{
    char path[512];
    if (read_self_exe_path(path, sizeof(path)) != 0) return;
    errno = 0;
    int fd = open(path, O_RDWR);
    int ok = (fd == -1 && errno == ETXTBSY);
    CHECK_OR_BUG(ok, "bug-open-etxtbsy-not-implemented", "ETXTBSY (c) real_path O_RDWR -> -1 ETXTBSY");
    if (fd >= 0) close(fd);
}

/* (d) 真实路径 + O_RDONLY|O_TRUNC → ETXTBSY（TRUNC 也算写访问）*/
static void etxtbsy_realpath_rdonly_trunc(void)
{
    char path[512];
    if (read_self_exe_path(path, sizeof(path)) != 0) return;
    errno = 0;
    int fd = open(path, O_RDONLY | O_TRUNC);
    int ok = (fd == -1 && errno == ETXTBSY);
    CHECK_OR_BUG(ok, "bug-open-etxtbsy-not-implemented", "ETXTBSY (d) real_path O_RDONLY|O_TRUNC -> -1 ETXTBSY");
    if (fd >= 0) close(fd);
}

/* (e) 真实路径 + O_RDONLY → 应成功（基线对照，ETXTBSY 仅作用于"写访问"）*/
static void etxtbsy_realpath_rdonly_ok(void)
{
    char path[512];
    if (read_self_exe_path(path, sizeof(path)) != 0) return;
    errno = 0;
    int fd = open(path, O_RDONLY);
    CHECK(fd >= 0,                                               "ETXTBSY (e) real_path O_RDONLY 基线 -> fd>=0（纯读应允许）");
    if (fd >= 0) close(fd);
}

int open_err_etxtbsy_run(void)
{
    printf("\n----- open_err_etxtbsy -----\n");
    cleanup_tree(M_DIR);
    CHECK(ensure_dir(M_DIR) == 0,                                "etxtbsy setup: mkdir");
    etxtbsy_procself_wronly();
    etxtbsy_realpath_wronly();
    etxtbsy_realpath_rdwr();
    etxtbsy_realpath_rdonly_trunc();
    etxtbsy_realpath_rdonly_ok();
    cleanup_tree(M_DIR);
    printf("  ----- open_err_etxtbsy: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
