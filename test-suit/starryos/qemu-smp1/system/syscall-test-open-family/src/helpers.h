#pragma once
/*
 * helpers.h —— test-open-family 共享路径宏 + helper 函数原型。
 *
 * 全模块共用的全局 setup 路径 (OF_*) 在 main.c 的 global_setup() 里建好；
 * 各模块在自己的 <module>_run() 开头另建 OF_MOD_<NAME> 私有目录，互不污染。
 */

#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <sys/types.h>
#include <sys/stat.h>

/* ── 全局共享只读路径（main.c global_setup() 建好）── */
#define OF_DIR       "/tmp/topen"
#define OF_REGULAR   OF_DIR "/regular"            /* 普通文件，内容 "hello" */
#define OF_SUBDIR    OF_DIR "/subdir"             /* 子目录 */
#define OF_SYMLINK   OF_DIR "/symlink_to_reg"     /* 指向 OF_REGULAR 的有效 symlink */
#define OF_DANGLING  OF_DIR "/dangling"           /* 指向不存在路径的悬空 symlink */
#define OF_SYM2DIR   OF_DIR "/symlink_to_dir"     /* 指向 OF_SUBDIR 的目录 symlink */

/* 各模块私有目录命名约定，避免互相污染 */
#define OF_MOD(name) "/tmp/topen_" name

/* CHECK_QUIET: 仅在 FAIL 时打印，PASS 时只增计数器（用于 20k+ 断言的大矩阵，
 * 避免 QEMU 串口被 PASS 行淹没拖慢）。要求宿主 TU 已 include test_framework.h
 * 以提供 __pass / __fail static 计数器。 */
#define CHECK_QUIET(cond, msg) do {                                     \
    if (cond) {                                                         \
        __pass++;                                                       \
    } else {                                                            \
        printf("  FAIL | %s:%d | %s | errno=%d (%s)\n",                \
               __FILE__, __LINE__, msg, errno, strerror(errno));        \
        __fail++;                                                       \
    }                                                                   \
} while(0)

/* ── 通用 helper（实现见 helpers.c）── */

/* 递归删除目录 path 下所有内容，再 rmdir(path)。失败静默忽略（防御性清理）。 */
void cleanup_tree(const char *path);

/* mkdir(path, 0755)；若已存在不报错。返回 0 OK / -1 fail。 */
int ensure_dir(const char *path);

/* 创建 path 内容为 content（长度 len）的普通文件，权限 mode。覆盖已存在。返回 0/-1。 */
int write_file(const char *path, const void *content, size_t len, mode_t mode);

/* 读 path 全部内容到 buf（最多 buf_size-1 字节，末尾置 \0）；返回读到字节数 / -1。 */
ssize_t read_file(const char *path, char *buf, size_t buf_size);

/* 取 path 的 stat.st_mode 低 12 位（07777 含 suid/sgid/sticky）；失败返 (mode_t)-1。 */
mode_t get_file_mode(const char *path);

/* 取 path 的 stat.st_size；失败返 -1。 */
off_t get_file_size(const char *path);

/* 文件存在且是普通文件返 1；否则 0。 */
int is_regular_file(const char *path);

/* 文件存在且是目录返 1；否则 0。 */
int is_directory(const char *path);
