#include "path_common.h"

#include <sys/wait.h>

/*
 * chroot(2) — change root directory.
 *
 * man 2 chroot:
 *   "chroot() changes the root directory of the calling process to that
 *    specified in path."
 *   "This call changes an ingredient in the pathname resolution process and
 *    does not change the current working directory."
 *   "On success, 0 is returned. On error, -1 is returned, and errno is set to
 *    indicate the error."
 *   "This call is privileged (Linux: requires CAP_SYS_CHROOT)."
 *
 * 测试覆盖（Linux 兼容最小集 + 权限优先级）：
 *   root 直接调用：
 *     (a) chroot("/") → 0
 *     (b) chroot(不存在路径) → -1 ENOENT
 *     (c) chroot(普通文件) → -1 ENOTDIR
 *   dropped-user 权限/优先级探针：
 *     (d) chroot("/") → -1 EPERM
 *     (e) chroot(不存在路径) → -1 ENOENT
 *     (f) chroot(普通文件) → -1 ENOTDIR
 *     (g) chroot(目标目录不可搜索) → -1 EACCES
 *     (h) chroot(父目录不可搜索) → -1 EACCES
 *
 * 未覆盖/不便实现（权限/环境依赖）：
 *   (i) chdir/cwd 与 chroot 的交互（需要更复杂的 cwd/文件描述符回退路径）
 */

static long raw_chroot(const char *path)
{
    return syscall(SYS_chroot, path);
}

struct chroot_case {
    const char *path;
    int exp_ret;
    int exp_errno;
    const char *desc;
};

struct chroot_probe_args {
    const char *path;
};

struct chroot_result_payload {
    long ret;
    int err;
};

static int probe_chroot_errno(void *arg)
{
    const struct chroot_probe_args *probe = (const struct chroot_probe_args *)arg;
    return raw_chroot(probe->path) == -1 ? errno : 0;
}

static void run_chroot_case_in_child(const struct chroot_case *tc)
{
    int pipefd[2];
    CHECK_RET(pipe(pipefd), 0, "chroot: create result pipe");

    pid_t pid = fork();
    CHECK(pid >= 0, "chroot: fork case runner");
    if (pid < 0) {
        close(pipefd[0]);
        close(pipefd[1]);
        return;
    }

    if (pid == 0) {
        struct chroot_result_payload payload;
        close(pipefd[0]);
        errno = 0;
        payload.ret = raw_chroot(tc->path);
        payload.err = payload.ret == -1 ? errno : 0;
        (void)write(pipefd[1], &payload, sizeof(payload));
        close(pipefd[1]);
        _exit(0);
    }

    close(pipefd[1]);
    struct chroot_result_payload payload = {0};
    ssize_t n = read(pipefd[0], &payload, sizeof(payload));
    int saved_errno = errno;
    close(pipefd[0]);

    int status = 0;
    CHECK_RET(waitpid(pid, &status, 0), pid, "chroot: wait case runner");
    if (n != (ssize_t)sizeof(payload)) {
        errno = n < 0 ? saved_errno : EIO;
        CHECK(0, "chroot: read result payload");
        return;
    }

    errno = payload.err;
    if (tc->exp_ret == 0) {
        CHECK(payload.ret == 0, tc->desc);
    } else {
        CHECK(payload.ret == -1 && payload.err == tc->exp_errno, tc->desc);
    }
}

static void run_dropped_chroot_case(const struct chroot_case *tc)
{
    struct chroot_probe_args probe = {
        .path = tc->path,
    };
    int probe_value = 0;
    int probe_status = path_run_as_dropped_user(&probe_value, probe_chroot_errno, &probe);
    CHECK(probe_status >= 0, "chroot: launch dropped-user probe");
    if (probe_status == PATH_DROP_PROBE_SETUID_FAILED) {
        CHECK(0, "chroot: setuid failed in child for dropped-user probe");
    } else if (probe_status == PATH_DROP_PROBE_OK) {
        if (probe_value != tc->exp_errno) {
            errno = probe_value > 0 ? probe_value : -probe_value;
        }
        CHECK(probe_value == tc->exp_errno, tc->desc);
    }
}

void test_chroot(void)
{
    uid_t euid = geteuid();
    char regfile[256];
    path_join(regfile, sizeof(regfile), "regfile");
    char no_such_root[256];
    char no_search_dir[256];
    char no_search_leaf[256];
    path_join(no_such_root, sizeof(no_such_root), "no_such_root_chroot");
    path_join(no_search_dir, sizeof(no_search_dir), "chroot_noexec_dir");
    path_join(no_search_leaf, sizeof(no_search_leaf), "chroot_noexec_parent/leaf");

    int dfd = open_base_dir();
    if (dfd < 0) {
        return;
    }

    int tfd = openat(dfd, "regfile", O_CREAT | O_RDWR | O_TRUNC, 0644);
    if (tfd >= 0) {
        close(tfd);
    }

    unlinkat(dfd, "no_such_root_chroot", 0);
    unlinkat(dfd, "no_such_root_chroot", AT_REMOVEDIR);

    struct path_perm_matrix_entry perm_entries[] = {
        {"chroot_noexec_dir", 0755, PATH_PERM_DIR},
        {"chroot_noexec_parent", 0755, PATH_PERM_DIR},
        {"chroot_noexec_parent/leaf", 0755, PATH_PERM_DIR},
    };
    path_cleanup_perm_matrix_at(dfd, perm_entries, sizeof(perm_entries) / sizeof(perm_entries[0]));
    CHECK_RET(path_setup_perm_matrix_at(dfd, perm_entries, sizeof(perm_entries) / sizeof(perm_entries[0])),
              0,
              "chroot: setup permission matrix");
    CHECK_RET(fchmodat(dfd, "chroot_noexec_dir", 0000, 0), 0, "chroot: chmod no-search dir");
    CHECK_RET(fchmodat(dfd, "chroot_noexec_parent", 0000, 0), 0, "chroot: chmod no-search parent");

    struct chroot_case root_cases[] = {
        {"/", 0, 0, "chroot: root chroot('/')"},
        {no_such_root, -1, ENOENT, "chroot: root missing path -> ENOENT"},
        {regfile, -1, ENOTDIR, "chroot: root regfile -> ENOTDIR"},
    };

    struct chroot_case dropped_cases[] = {
        {"/", -1, EPERM, "chroot: dropped user chroot('/') -> EPERM"},
        {no_such_root, -1, ENOENT, "chroot: dropped user missing path -> ENOENT"},
        {regfile, -1, ENOTDIR, "chroot: dropped user regfile -> ENOTDIR"},
        {no_search_dir, -1, EACCES, "chroot: dropped user target dir without search permission -> EACCES"},
        {no_search_leaf, -1, EACCES, "chroot: dropped user parent dir without search permission -> EACCES"},
    };

    if (euid == 0) {
        for (size_t i = 0; i < sizeof(root_cases) / sizeof(root_cases[0]); i++) {
            run_chroot_case_in_child(&root_cases[i]);
        }

        for (size_t i = 0; i < sizeof(dropped_cases) / sizeof(dropped_cases[0]); i++) {
            run_dropped_chroot_case(&dropped_cases[i]);
        }
    } else {
        for (size_t i = 0; i < sizeof(dropped_cases) / sizeof(dropped_cases[0]); i++) {
            run_chroot_case_in_child(&dropped_cases[i]);
        }
    }

    CHECK_RET(fchmodat(dfd, "chroot_noexec_parent", 0755, 0), 0, "chroot: restore no-search parent");
    CHECK_RET(fchmodat(dfd, "chroot_noexec_dir", 0755, 0), 0, "chroot: restore no-search dir");
    path_cleanup_perm_matrix_at(dfd, perm_entries, sizeof(perm_entries) / sizeof(perm_entries[0]));
    unlinkat(dfd, "regfile", 0);
    close(dfd);
}
