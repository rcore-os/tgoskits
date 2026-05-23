#include "path_common.h"

/*
 * fchdir(2) — change working directory.
 *
 * man 2 fchdir:
 *   "fchdir() changes the current working directory of the calling process to
 *    the directory referred to by the open file descriptor fd."
 *
 * 测试覆盖（Linux 兼容最小集，权限场景要求 root）：
 *   (a) fd=目录fd → 0，getcwd 反映新工作目录
 *   (b) fd=-1 → -1 EBADF
 *   (c) fd=普通文件fd → -1 ENOTDIR
 *   (d) root + setuid 降权：由降权用户自己创建 0400 目录并打开，再 fchdir
 *       → -1 EACCES
 *
 */

struct basic_test_case {
    int fd;
    int exp_ret;
    int exp_errno;
    const char *exp_cwd;
    const char *desc;
};

struct eacces_test_case {
    const char *parent_dir;
    const char *dir_name;
    int exp_errno;
    const char *desc;
};

static int probe_fchdir_eacces(void *arg)
{
    struct eacces_test_case *probe = (struct eacces_test_case *)arg;
    char path[256];
    snprintf(path, sizeof(path), "%s/%s", probe->parent_dir, probe->dir_name);

    if (mkdir(path, 0400) != 0) {
        return -errno;
    }

    int fd = open(path, O_RDONLY | O_DIRECTORY);
    if (fd < 0) {
        int saved_errno = errno;
        rmdir(path);
        return -saved_errno;
    }

    errno = 0;
    int result = fchdir(fd) == -1 ? errno : 0;
    close(fd);
    rmdir(path);
    return result;
}

static void run_basic_test_cases(const struct basic_test_case *test_cases, size_t count)
{
    for (size_t i = 0; i < count; i++) {
        const struct basic_test_case *tc = &test_cases[i];

        errno = 0;
        int r = fchdir(tc->fd);
        if (tc->exp_ret == 0) {
            CHECK_RET(r, 0, tc->desc);
            char cwd[512];
            CHECK(getcwd(cwd, sizeof(cwd)) != NULL, "fchdir: getcwd after fchdir");
            CHECK(strcmp(cwd, tc->exp_cwd) == 0, "fchdir: cwd equals expected");
            CHECK_RET(chdir(PATH_FAMILY_BASE), 0, "fchdir: reset to BASE");
        } else {
            CHECK(r == -1 && errno == tc->exp_errno, tc->desc);
        }
    }
}

static void run_eacces_test_case(struct eacces_test_case *tc)
{
    int probe_value = 0;
    int probe_status = path_run_as_dropped_user(&probe_value, probe_fchdir_eacces, tc);
    CHECK(probe_status >= 0, "fchdir: launch dropped-user EACCES probe");
    if (probe_status == PATH_DROP_PROBE_SETUID_FAILED) {
        CHECK(0, "fchdir: setuid failed in child for EACCES probe");
    } else if (probe_status == PATH_DROP_PROBE_OK) {
        CHECK(probe_value == tc->exp_errno, tc->desc);
    }
}

void test_fchdir(void)
{
    char old_cwd[512];
    CHECK(getcwd(old_cwd, sizeof(old_cwd)) != NULL, "fchdir: capture old cwd");

    if (geteuid() != 0) {
        printf("  SKIP | %s:%d | fchdir: needs_root=1 for permission coverage\n",
               __FILE__,
               __LINE__);
        return;
    }

    CHECK_RET(chdir(PATH_FAMILY_BASE), 0, "fchdir: chdir(BASE)");

    char target_name[] = "fchdir_target";
    char drop_parent[] = "fchdir_drop_parent";
    char expect_target[512];
    snprintf(expect_target, sizeof(expect_target), "%s/%s", PATH_FAMILY_BASE, target_name);

    struct path_perm_matrix_entry perm_entries[] = {
        {target_name, 0755, PATH_PERM_DIR},
        {drop_parent, 0777, PATH_PERM_DIR},
        {"regfile", 0644, PATH_PERM_FILE},
    };
    path_cleanup_perm_matrix_at(AT_FDCWD, perm_entries, sizeof(perm_entries) / sizeof(perm_entries[0]));
    CHECK_RET(path_setup_perm_matrix_at(AT_FDCWD, perm_entries, sizeof(perm_entries) / sizeof(perm_entries[0])),
              0,
              "fchdir: setup permission matrix");

    int dirfd = open(target_name, O_RDONLY | O_DIRECTORY);
    CHECK(dirfd >= 0, "fchdir: open target dir");

    int filefd = open("regfile", O_RDONLY);
    CHECK(filefd >= 0, "fchdir: open regfile");

    struct basic_test_case test_cases[] = {
        /* (a) 目录fd 成功 */
        {dirfd, 0, 0, expect_target, "fchdir: change to dir fd"},
        /* (b) fd=-1 */
        {-1, -1, EBADF, NULL, "fchdir: fd=-1 -> EBADF"},
        /* (c) 普通文件fd */
        {filefd, -1, ENOTDIR, NULL, "fchdir: regfile fd -> ENOTDIR"},
    };
    run_basic_test_cases(test_cases, sizeof(test_cases) / sizeof(test_cases[0]));

    struct eacces_test_case eacces_case = {
        .parent_dir = drop_parent,
        .dir_name = "fchdir_eacces_0400",
        .exp_errno = EACCES,
        .desc = "fchdir: dropped user creates 0400 dir, opens it, fchdir -> EACCES",
    };
    run_eacces_test_case(&eacces_case);

    if (dirfd >= 0) {
        close(dirfd);
    }
    if (filefd >= 0) {
        close(filefd);
    }
    unlinkat(AT_FDCWD, "fchdir_drop_parent/fchdir_eacces_0400", AT_REMOVEDIR);
    path_cleanup_perm_matrix_at(AT_FDCWD, perm_entries, sizeof(perm_entries) / sizeof(perm_entries[0]));
    chdir(old_cwd);
}
