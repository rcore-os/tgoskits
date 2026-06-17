/* procfs_visibility.c — setreuid/setregid 后 /proc/self/status 同步反映.
 *
 * man proc(5) §/proc/[pid]/status:
 *   Uid:\t<real>\t<effective>\t<saved>\t<fs>
 *
 * man 2 setreuid §"saved set-user-ID auto-update":
 *   "If the real user ID is set (i.e., ruid is not -1) or the effective user
 *    ID is set to a value not equal to the previous real user ID, the saved
 *    set-user-ID will be set to the new effective user ID."
 *
 * 5 维度覆盖 (a-e):
 *   (a) setreuid(1000, 2000) — ruid set → suid 应跟 new euid = 2000
 *   (b) setreuid(-1, 3000) NOCHG ruid, euid != old r → suid 跟 new euid
 *   (c) setreuid(-1, 0) NOCHG ruid, euid == old r → suid NOT updated (keep old)
 *   (d) setregid 镜像 (a)
 *   (e) fsuid follow: setreuid 后 procfs Uid 第 4 字段 (fs) == new euid
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

/* (a) setreuid(1000, 2000) ruid set → suid = new e = 2000 */
static void proc_uid_after_setreuid_full(void)
{
    /* 测什么: man setreuid §saved rule — ruid 被设 (!= -1) → suid = new e.
     * 怎么测: fork → child setreuid(1000, 2000) → 读 Uid 行.
     * 期望: r=1000, e=2000, s=2000 (跟 new e), fs=2000.
     * 为什么: 验 starry saved-set-uid 规则 (RE-D5) 在 procfs 正确反映. */
    if (getuid() != 0) { printf("  procfs (a) skip\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        if (setreuid(1000, 2000) != 0) _exit(99);
        uint32_t r, e, s, fs;
        if (parse_proc_id_line("Uid:", &r, &e, &s, &fs) != 0) _exit(98);
        if (r != 1000 || e != 2000 || s != 2000 || fs != 2000) {
            printf("  got %u %u %u %u\n", r, e, s, fs);
            _exit(1);
        }
        _exit(0);
    }
    int status;
    waitpid_safely_pv(pid, &status);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "procfs (a) setreuid(1k,2k) → 1000 2000 2000 2000 (saved 跟 new e)");
}

/* (b) setreuid(-1, 3000) ruid NOCHG, euid != old r=0 → saved updates */
static void proc_uid_after_setreuid_nochg_r(void)
{
    /* 测什么: NOCHG ruid 路径下若 new euid != prev real → saved 仍跟 new euid.
     * 怎么测: fork → child setreuid(-1, 3000) (old r=0, new e=3000 != 0).
     * 期望: r=0, e=3000, s=3000, fs=3000.
     * 为什么: 验 RE-D5 第二条件 (euid != prev r). */
    if (getuid() != 0) { printf("  procfs (b) skip\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        if (setreuid((uid_t)-1, 3000) != 0) _exit(99);
        uint32_t r, e, s, fs;
        if (parse_proc_id_line("Uid:", &r, &e, &s, &fs) != 0) _exit(98);
        if (r != 0 || e != 3000 || s != 3000 || fs != 3000) {
            printf("  got %u %u %u %u\n", r, e, s, fs);
            _exit(1);
        }
        _exit(0);
    }
    int status;
    waitpid_safely_pv(pid, &status);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "procfs (b) setreuid(-1,3k) (e!=prev_r) → 0 3000 3000 3000");
}

/* (c) setreuid(-1, 0) NOCHG ruid, euid == old r=0 → saved NOT updated */
static void proc_uid_after_setreuid_nochg_same(void)
{
    /* 测什么: NOCHG ruid + new euid == prev real → saved 不更 (RE-D5 反向).
     * 怎么测: fork → child setreuid(-1, 0) (old r=0, new e=0 == prev r).
     * 期望: r=0, e=0, s=0 (启动时 0), fs=0.
     * 注: 这个 case 在 baseline 状态下 saved 就是 0, "不更" 和"更" 结果相同.
     *     要真区分需要先 setreuid 改 s, 再这样调.
     * 为什么: 验 RE-D5 反向 case (条件都不满足 → saved 不更). */
    if (getuid() != 0) { printf("  procfs (c) skip\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        /* 先 setresuid(0, 0, 5000) — saved 设为 5000.
         * 然后 setreuid(-1, 0) — 不应更 saved (仍 5000). */
        if (setresuid(0, 0, 5000) != 0) _exit(99);
        if (setreuid((uid_t)-1, 0) != 0) _exit(98);
        uint32_t r, e, s, fs;
        if (parse_proc_id_line("Uid:", &r, &e, &s, &fs) != 0) _exit(97);
        if (r != 0 || e != 0 || s != 5000 || fs != 0) {
            printf("  got %u %u %u %u\n", r, e, s, fs);
            _exit(1);
        }
        _exit(0);
    }
    int status;
    waitpid_safely_pv(pid, &status);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "procfs (c) setresuid(0,0,5k)+setreuid(-1,0) → s 仍 5000 (NOT updated)");
}

/* (d) setregid(1000, 2000) 镜像 (a) */
static void proc_gid_after_setregid_full(void)
{
    /* 测什么: 镜像 (a) — RE-D5 saved rule 对 gid 同样成立.
     * 怎么测: fork → child setregid(1000, 2000) → 读 Gid 行.
     * 期望: r=1000, e=2000, s=2000, fs=2000. */
    if (getuid() != 0) { printf("  procfs (d) skip\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        if (setregid(1000, 2000) != 0) _exit(99);
        uint32_t r, e, s, fs;
        if (parse_proc_id_line("Gid:", &r, &e, &s, &fs) != 0) _exit(98);
        if (r != 1000 || e != 2000 || s != 2000 || fs != 2000) _exit(1);
        _exit(0);
    }
    int status;
    waitpid_safely_pv(pid, &status);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "procfs (d) setregid(1k,2k) → 1000 2000 2000 2000");
}

/* (e) 复合: setregid + setreuid — uid/gid 独立 */
static void proc_compound_setre(void)
{
    /* 测什么: setreuid 不串扰 gid, setregid 不串扰 uid.
     * 怎么测: fork → child setregid + setreuid → 同时验.
     * 期望: 两行精确. */
    if (getuid() != 0) { printf("  procfs (e) skip\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        if (setregid(100, 200) != 0) _exit(99);
        if (setreuid(300, 400) != 0) _exit(98);
        uint32_t ur, ue, us, ufs;
        uint32_t gr, ge, gs, gfs;
        if (parse_proc_id_line("Uid:", &ur, &ue, &us, &ufs) != 0) _exit(97);
        if (parse_proc_id_line("Gid:", &gr, &ge, &gs, &gfs) != 0) _exit(96);
        if (ur != 300 || ue != 400 || us != 400 || ufs != 400) _exit(1);
        if (gr != 100 || ge != 200 || gs != 200 || gfs != 200) _exit(2);
        _exit(0);
    }
    int status;
    waitpid_safely_pv(pid, &status);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "procfs (e) compound setregid + setreuid (uid/gid 独立)");
}

int procfs_visibility_run(void)
{
    printf("\n----- procfs_visibility -----\n");
    proc_uid_after_setreuid_full();
    proc_uid_after_setreuid_nochg_r();
    proc_uid_after_setreuid_nochg_same();
    proc_gid_after_setregid_full();
    proc_compound_setre();
    printf("  ----- procfs_visibility: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
