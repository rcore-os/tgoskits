/* procfs_visibility.c — setresuid/setresgid 后 /proc/self/status 同步反映.
 *
 * man proc(5) §/proc/[pid]/status:
 *   "Uid: Real, effective, saved set, and filesystem UIDs"
 *   格式: Uid:\t<real>\t<effective>\t<saved>\t<fs>
 *
 * 价值: starry **不支持 setfsuid/setfsgid syscall** → 无法直接 query fsuid.
 *       /proc/self/status 是间接观察 fsuid 的 唯一 user-space 路径.
 *       此模块替代 fsuid_follow.c 中无法验证的部分.
 *
 * man 2 setresuid §"DESCRIPTION":
 *   "Regardless of what changes are made to the real UID, effective UID, and
 *    saved set-user-ID, the filesystem UID is always set to the same value
 *    as the (possibly new) effective UID."
 *
 * 6 维度覆盖 (a-f):
 *   (a) baseline: root Uid 0 0 0 0
 *   (b) setresuid(1000, 2000, 3000) → Uid 1000 2000 3000 2000 (fs == e)
 *   (c) setresuid(-1, 4000, -1) NOCHG mixed → Uid r 4000 s 4000 (fs 跟 new e)
 *   (d) setresgid(1000, 2000, 3000) → Gid 1000 2000 3000 2000
 *   (e) setresuid 后 setresgid 复合 → Uid + Gid 都正确 + uid 不串扰 gid
 *   (f) baseline Gid: 0 0 0 0
 */

#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static int waitpid_safely_pv(pid_t pid, int *status)
{
    pid_t r = waitpid(pid, status, 0);
    return r == pid ? 0 : -1;
}

/* 解析 /proc/self/status 的 "Uid:" / "Gid:" 行 → 4 字段 */
static int parse_proc_id_line(const char *prefix,
                               uint32_t *r, uint32_t *e, uint32_t *s, uint32_t *fs)
{
    FILE *f = fopen("/proc/self/status", "r");
    if (!f) return -1;
    char line[256];
    int found = -1;
    while (fgets(line, sizeof line, f)) {
        if (strncmp(line, prefix, strlen(prefix)) == 0) {
            if (sscanf(line + strlen(prefix), "%u %u %u %u", r, e, s, fs) == 4)
                found = 0;
            break;
        }
    }
    fclose(f);
    return found;
}

/* (a) baseline root */
static void proc_uid_baseline(void)
{
    /* 测什么: man proc(5) — root 启动 Uid 0 0 0 0.
     * 怎么测: 读 /proc/self/status Uid 行.
     * 期望: r=e=s=fs=0. */
    if (getuid() != 0) { printf("  procfs (a) skip: needs root\n"); return; }
    uint32_t r, e, s, fs;
    int rc = parse_proc_id_line("Uid:", &r, &e, &s, &fs);
    CHECK(rc == 0 && r == 0 && e == 0 && s == 0 && fs == 0,
          "procfs (a) baseline root: Uid 0 0 0 0");
}

/* (b) setresuid(1000,2000,3000) — 验三参数独立 + fsuid 跟 euid */
static void proc_uid_after_setresuid(void)
{
    /* 测什么: man setresuid §D5 — fsuid 总跟 new euid.
     * 怎么测: fork → child setresuid(1000,2000,3000) → 读 Uid 行.
     * 期望: 1000 2000 3000 2000 (fs == e).
     * 为什么: starry 无 setfsuid syscall, 用 procfs 验 fsuid 跟随是唯一办法. */
    if (getuid() != 0) { printf("  procfs (b) skip\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresuid(1000, 2000, 3000) != 0) _exit(99);
        uint32_t r, e, s, fs;
        if (parse_proc_id_line("Uid:", &r, &e, &s, &fs) != 0) _exit(98);
        if (r != 1000 || e != 2000 || s != 3000 || fs != 2000) {
            printf("  got %u %u %u %u\n", r, e, s, fs);
            _exit(1);
        }
        _exit(0);
    }
    int status;
    waitpid_safely_pv(pid, &status);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "procfs (b) setresuid(1k,2k,3k) → Uid 1000 2000 3000 2000 (fs==e)");
}

/* (c) NOCHG mixed: setresuid(-1, 4000, -1) — 只改 euid, fs 应跟 euid 变 */
static void proc_uid_after_setresuid_nochg(void)
{
    /* 测什么: NOCHG 路径下 fs 仍随 new euid (即使 r/s 不变).
     * 怎么测: fork → child setresuid(-1, 4000, -1) → 读 Uid 行.
     * 期望: r 不变 (0), e=4000, s 不变 (0 启动), fs=4000.
     * 为什么: 验 fsuid 严格 = new euid (即便 NOCHG 路径不动 r/s 也要更 fs). */
    if (getuid() != 0) { printf("  procfs (c) skip\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresuid((uid_t)-1, 4000, (uid_t)-1) != 0) _exit(99);
        uint32_t r, e, s, fs;
        if (parse_proc_id_line("Uid:", &r, &e, &s, &fs) != 0) _exit(98);
        /* 启动时 0 0 0 0. NOCHG r/s 后: 0 4000 0 4000 */
        if (r != 0 || e != 4000 || s != 0 || fs != 4000) {
            printf("  got %u %u %u %u\n", r, e, s, fs);
            _exit(1);
        }
        _exit(0);
    }
    int status;
    waitpid_safely_pv(pid, &status);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "procfs (c) setresuid(-1,4000,-1) → 0 4000 0 4000 (fs 跟 new e)");
}

/* (d) setresgid(1000,2000,3000) */
static void proc_gid_after_setresgid(void)
{
    /* 测什么/怎么测/期望/为什么: 镜像 (b) — fsgid 跟 new egid. */
    if (getuid() != 0) { printf("  procfs (d) skip\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresgid(1000, 2000, 3000) != 0) _exit(99);
        uint32_t r, e, s, fs;
        if (parse_proc_id_line("Gid:", &r, &e, &s, &fs) != 0) _exit(98);
        if (r != 1000 || e != 2000 || s != 3000 || fs != 2000) {
            printf("  got %u %u %u %u\n", r, e, s, fs);
            _exit(1);
        }
        _exit(0);
    }
    int status;
    waitpid_safely_pv(pid, &status);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "procfs (d) setresgid(1k,2k,3k) → Gid 1000 2000 3000 2000");
}

/* (e) 复合 setresuid + setresgid — 不串扰 */
static void proc_compound_setres(void)
{
    /* 测什么: setresuid 不影响 Gid, setresgid 不影响 Uid (cred 独立).
     * 怎么测: fork → child setresuid + setresgid → 同时验 Uid + Gid.
     * 期望: 两行各自独立精确.
     * 为什么: 防 starry cred 内部 uid/gid 字段串扰 (历史 microkernel bug 常见). */
    if (getuid() != 0) { printf("  procfs (e) skip\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        /* 必须先 setresgid (root 时 cap 在) 再 setresuid (清 cap).
         * 顺序反了 → setresgid 在 cap 丢后失败 EPERM. */
        if (setresgid(400, 500, 600) != 0) _exit(99);
        if (setresuid(100, 200, 300) != 0) _exit(98);
        uint32_t ur, ue, us, ufs;
        uint32_t gr, ge, gs, gfs;
        if (parse_proc_id_line("Uid:", &ur, &ue, &us, &ufs) != 0) _exit(97);
        if (parse_proc_id_line("Gid:", &gr, &ge, &gs, &gfs) != 0) _exit(96);
        if (ur != 100 || ue != 200 || us != 300 || ufs != 200) _exit(1);
        if (gr != 400 || ge != 500 || gs != 600 || gfs != 500) _exit(2);
        _exit(0);
    }
    int status;
    waitpid_safely_pv(pid, &status);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "procfs (e) compound setresuid + setresgid (uid/gid 独立)");
}

/* (f) baseline Gid */
static void proc_gid_baseline(void)
{
    /* 测什么/怎么测/期望/为什么: 启动 Gid 行 0 0 0 0 (baseline). */
    if (getuid() != 0) { printf("  procfs (f) skip\n"); return; }
    uint32_t r, e, s, fs;
    int rc = parse_proc_id_line("Gid:", &r, &e, &s, &fs);
    CHECK(rc == 0 && r == 0 && e == 0 && s == 0 && fs == 0,
          "procfs (f) baseline root: Gid 0 0 0 0");
}

int procfs_visibility_run(void)
{
    printf("\n----- procfs_visibility -----\n");
    proc_uid_baseline();
    proc_uid_after_setresuid();
    proc_uid_after_setresuid_nochg();
    proc_gid_after_setresgid();
    proc_compound_setres();
    proc_gid_baseline();
    printf("  ----- procfs_visibility: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
