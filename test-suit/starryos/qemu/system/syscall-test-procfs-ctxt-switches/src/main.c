#define _GNU_SOURCE
#include "test_framework.h"
#include <stdint.h>
#include <unistd.h>

/*
 * /proc/<pid>/status 上下文切换计数回归测试 — 对应 PR #1168 (fix-tui-procfs-ioctl)。
 *
 * 触发背景 (为什么写这个测例):
 *   htop / glances 解析 /proc/<pid>/status 的 voluntary_ctxt_switches /
 *   nonvoluntary_ctxt_switches 来显示每进程上下文切换数。starry 此前 status
 *   缺这两行 -> TUI 解析得不到字段。补 stub 行后可被解析。
 *
 * man 5 proc §/proc/[pid]/status:
 *   "voluntary_ctxt_switches, nonvoluntary_ctxt_switches: Number of voluntary
 *    and involuntary context switches (since Linux 2.6.23)."
 *   格式: "<name>:\t<number>"。
 *
 * starry 实现 (kernel/src/pseudofs/proc.rs): status 末尾追加两行
 *   voluntary_ctxt_switches:\t<n>  /  nonvoluntary_ctxt_switches:\t<n>。
 *
 * 修复前: 字段不存在 -> 找不到 -> FAIL。
 * 修复后: 两字段都存在且能解析为非负整数 -> PASS。
 *
 * 同时覆盖 /proc/self/status 与 /proc/<getpid()>/status 两种路径。
 */

/* 在 status 文件里查找 "<name>:" 行并把其后的整数解析到 *out。找到且可解析返回 0。 */
static int find_status_field(const char *path, const char *name, unsigned long *out)
{
    FILE *f = fopen(path, "r");
    if (!f)
        return -1;
    char line[256];
    int rc = -1;
    size_t nlen = strlen(name);
    while (fgets(line, sizeof line, f)) {
        if (strncmp(line, name, nlen) == 0 && line[nlen] == ':') {
            if (sscanf(line + nlen + 1, "%lu", out) == 1)
                rc = 0;
            break;
        }
    }
    fclose(f);
    return rc;
}

static void check_path(const char *path)
{
    unsigned long v = 0;
    char msg[160];

    int rc = find_status_field(path, "voluntary_ctxt_switches", &v);
    snprintf(msg, sizeof msg, "%s: voluntary_ctxt_switches 存在且可解析为整数 (=%lu)",
             path, v);
    CHECK(rc == 0, msg);

    v = 0;
    rc = find_status_field(path, "nonvoluntary_ctxt_switches", &v);
    snprintf(msg, sizeof msg, "%s: nonvoluntary_ctxt_switches 存在且可解析为整数 (=%lu)",
             path, v);
    CHECK(rc == 0, msg);
}

int main(void)
{
    TEST_START("/proc/<pid>/status ctxt_switches");

    check_path("/proc/self/status");

    char pidpath[64];
    snprintf(pidpath, sizeof pidpath, "/proc/%d/status", (int)getpid());
    check_path(pidpath);

    TEST_DONE();
}
