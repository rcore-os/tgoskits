#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdio.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

/* saved_id_semantics — setreuid/setregid 的 saved-set-id 自动更新规则。
 *
 * man 2 setreuid §"...":
 *   "If the real user ID is set (i.e., ruid is not -1) or the effective user
 *    ID is set to a value not equal to the previous real user ID, the saved
 *    set-user-ID will be set to the new effective user ID."
 *
 * 三种情况导致 suid 被设为 new.euid（其他情况 suid 不变）：
 *   (1) ruid 被设（ruid != -1）
 *   (2) euid 被设且 new.euid != old.ruid
 *   (3) (1) 和 (2) 同时
 *
 * 否则 suid 保持不变。
 *
 * 测试 root setreuid 后 suid 是否如规则更新（关键不变量）：
 *   - root setreuid(2000, -1)         — ruid 设；suid 应更新为 new.euid（=old.euid）
 *   - root setreuid(-1, 3000)         — euid 设到非 old.ruid 的值；suid 应更新为 3000
 *   - root setreuid(-1, current_euid) — euid 设到 == current_euid（== old.ruid 在 root 启动时）；suid 不变
 *   - root setreuid(0, 0)             — ruid+euid 都设到 root；suid 应更新为 0
 */

static int waitpid_safely(pid_t pid, int *status)
{
    pid_t r = waitpid(pid, status, 0);
    return r == pid ? 0 : -1;
}

static void saved_id_ruid_set_updates_suid(void)
{
    if (getuid() != 0) {
        printf("  saved_id (a) skip: requires root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        /* 启动 r=e=s=0 */
        /* setreuid(2000, -1)：ruid 设；euid 不变 (=0)；suid 应更新为 new.euid = 0 */
        if (setreuid(2000, (uid_t)-1) != 0) _exit(1);
        uid_t r, e, s;
        if (getresuid(&r, &e, &s) != 0) _exit(2);
        /* r=2000 e=0 s=0（updated to new.euid=0；其实没变）*/
        if (r == 2000 && e == 0 && s == 0) _exit(0);
        _exit(3);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "saved_id (a) setreuid(2000, -1) by root → r=2000 e=0 s=0 (suid updated to new.euid)");
        }
    }
}

static void saved_id_euid_set_to_nonruid_updates_suid(void)
{
    if (getuid() != 0) {
        printf("  saved_id (b) skip\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        /* 启动 r=e=s=0 */
        /* setreuid(-1, 3000)：euid 设到 3000 != old.ruid (=0)；suid 应更新为 3000 */
        if (setreuid((uid_t)-1, 3000) != 0) _exit(1);
        uid_t r, e, s;
        if (getresuid(&r, &e, &s) != 0) _exit(2);
        if (r == 0 && e == 3000 && s == 3000) _exit(0);
        _exit(3);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "saved_id (b) setreuid(-1, 3000) by root: r=0 e=3000 s=3000 (suid updated to euid)");
        }
    }
}

static void saved_id_both_set_root(void)
{
    if (getuid() != 0) {
        printf("  saved_id (c) skip\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setreuid(0, 0) != 0) _exit(1);
        uid_t r, e, s;
        if (getresuid(&r, &e, &s) != 0) _exit(2);
        if (r == 0 && e == 0 && s == 0) _exit(0);
        _exit(3);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "saved_id (c) setreuid(0, 0) by root: r=e=s=0");
        }
    }
}

static void saved_id_both_nochg_no_change(void)
{
    /* setreuid(-1, -1)：什么都没设；suid 应保持原值 — 不被错误置 0 */
    uid_t r0, e0, s0;
    getresuid(&r0, &e0, &s0);
    if (setreuid((uid_t)-1, (uid_t)-1) != 0) {
        CHECK(0, "saved_id (d) setreuid(-1, -1) failed");
        return;
    }
    uid_t r1, e1, s1;
    getresuid(&r1, &e1, &s1);
    CHECK(r0 == r1 && e0 == e1 && s0 == s1,
          "saved_id (d) setreuid(-1, -1): all 3 IDs unchanged (no spurious suid update)");
}

/* (e) D5 反向 corner: ruid=-1 AND new.euid == old.ruid → suid 不变.
 *
 * man D5 的完整规则:
 *   "If the real user ID is set (ruid != -1) OR the effective user ID is
 *    set to a value NOT equal to the previous real user ID, the saved
 *    set-user-ID will be set to the new effective user ID."
 *
 * 反向: 两个条件都不满足 → suid 不更新.
 * 即: ruid == -1 (NOCHG) AND new.euid == old.ruid → suid 保持原值.
 *
 * 启动 r=e=s=0 时这个反向测不出 — 需要先 setresuid 让 r != s.
 * 设: r=0, e=2000, s=2000 → setreuid(-1, 0):
 *   - ruid=-1 → 条件 A 不满足
 *   - new.euid=0 == old.ruid=0 → 条件 B 不满足
 *   → suid 应保持 2000 (不被更新到 new.euid=0).
 *
 * Linux 行为: r=0 e=0 s=2000 (suid 保留 2000)
 * starry 行为: 若 D5 实现错误 (任意 setreuid 都改 suid), s 会变成 0.
 */
static void saved_id_d5_reverse_corner(void)
{
    if (getuid() != 0) {
        printf("  saved_id (e) skip: requires root to seed r=0 e=2000 s=2000\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        /* seed: r=0 e=2000 s=2000 (使 r != s 才能测 D5 反向) */
        if (setresuid(0, 2000, 2000) != 0) _exit(99);
        uid_t r0, e0, s0;
        if (getresuid(&r0, &e0, &s0) != 0) _exit(98);
        if (r0 != 0 || e0 != 2000 || s0 != 2000) _exit(97);

        /* setreuid(-1, 0): ruid=NOCHG, new.euid=0 == old.ruid=0 → 两条件都不满足
         * 此处必须 unpriv 路径 (euid=2000 时无 CAP_SETUID), euid 0 在 {r=0,s=2000} 集合内 OK */
        if (setreuid((uid_t)-1, 0) != 0) _exit(96);

        uid_t r1, e1, s1;
        if (getresuid(&r1, &e1, &s1) != 0) _exit(95);
        /* 期望: r=0 (不变), e=0 (设了), s=2000 (D5 不触发, 保留) */
        if (r1 == 0 && e1 == 0 && s1 == 2000) _exit(0);  /* Linux 行为 */
        if (s1 == 0) _exit(20);  /* starry bug: 误改 suid → 0 */
        _exit(1);
    }
    int status;
    if (waitpid_safely(pid, &status) == 0) {
        int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
        if (ec == 0)        CHECK(1, "saved_id (e) D5 反向: setreuid(-1, old_ruid) 不改 suid (Linux 行为 r=0 e=0 s=2000)");
        else if (ec == 20)  CHECK(0, "saved_id (e) FAIL: starry 误更新 suid (D5 反向条件未实现)");
        else                CHECK(0, "saved_id (e) failed (ec != 0/20)");
    }
}

/* (f) D5 反向 GID 维度: setregid(-1, old_rgid) → sgid 不变.
 *     镜像 (e) 但 GID 维度, 验 sys_setregid 同样实现 D5 反向. */
static void saved_id_d5_reverse_corner_gid(void)
{
    if (getuid() != 0) {
        printf("  saved_id (f) skip: requires root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        /* seed: rgid=0, egid=2000, sgid=2000 */
        if (setresgid(0, 2000, 2000) != 0) _exit(99);
        gid_t r0, e0, s0;
        if (getresgid(&r0, &e0, &s0) != 0) _exit(98);
        if (r0 != 0 || e0 != 2000 || s0 != 2000) _exit(97);

        /* setregid(-1, 0): egid=0 == old.rgid=0 → sgid 应保留 2000 */
        if (setregid((gid_t)-1, 0) != 0) _exit(96);

        gid_t r1, e1, s1;
        if (getresgid(&r1, &e1, &s1) != 0) _exit(95);
        if (r1 == 0 && e1 == 0 && s1 == 2000) _exit(0);
        if (s1 == 0) _exit(20);
        _exit(1);
    }
    int status;
    if (waitpid_safely(pid, &status) == 0) {
        int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
        if (ec == 0)        CHECK(1, "saved_id (f) D5 反向 GID: setregid(-1, old_rgid) 不改 sgid");
        else if (ec == 20)  CHECK(0, "saved_id (f) FAIL: starry 误更新 sgid (D5 反向 GID 未实现)");
        else                CHECK(0, "saved_id (f) failed");
    }
}

int saved_id_semantics_run(void)
{
    printf("\n----- saved_id_semantics -----\n");
    saved_id_ruid_set_updates_suid();
    saved_id_euid_set_to_nonruid_updates_suid();
    saved_id_both_set_root();
    saved_id_both_nochg_no_change();
    saved_id_d5_reverse_corner();      /* (e) Round 7-C — D5 反向 UID */
    saved_id_d5_reverse_corner_gid();  /* (f) Round 7-C — D5 反向 GID */
    printf("  ----- saved_id_semantics: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
