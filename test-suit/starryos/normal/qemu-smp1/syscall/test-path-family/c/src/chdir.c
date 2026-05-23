#include "path_common.h"

#include <limits.h>

#ifndef NAME_MAX
#define NAME_MAX 255
#endif

/*
 * chdir(2) — change working directory.
 *
 * man 2 chdir:
 *   "chdir() changes the current working directory of the calling process to
 *    the directory specified in path."
 *
 * 测试覆盖：
 *   (a) chdir("chdir_target") → 0，getcwd 反映新工作目录
 *   (b) chdir(".") → 0
 *   (c) chdir("..") → 0
 *   (d) chdir("/") → 0
 *   (e) chdir(不存在路径) → -1 ENOENT
 *   (f) chdir(普通文件) → -1 ENOTDIR
 *   (g) chdir(符号链接循环/过深) → -1 ELOOP
 *   (h) root + setuid 降权：目标目录无执行权限 → -1 EACCES
 *   (i) 单路径分量超过 NAME_MAX → -1 ENAMETOOLONG
 *
 * 未覆盖/不便实现（环境/权限依赖）：
 *   (j) 整体路径长度边界（< PATH_MAX 全覆盖）需要更大构造与时间预算
 */

struct basic_test_case {
    const char *name;
    int exp_ret;
    int exp_errno;
    const char *exp_cwd;
    const char *desc;
};

struct eacces_test_case {
    const char *parent_dir;
    const char *dir_name;
    mode_t mode;
    int exp_errno;
    const char *desc;
};

static int probe_chdir_eacces(void *arg)
{
    struct eacces_test_case *tc = (struct eacces_test_case *)arg;
    char path[512];
    snprintf(path, sizeof(path), "%s/%s", tc->parent_dir, tc->dir_name);

    if (mkdir(path, 0755) != 0) {
        return -errno;
    }
    if (chmod(path, tc->mode) != 0) {
        int saved_errno = errno;
        rmdir(path);
        return -saved_errno;
    }

    errno = 0;
    int result = chdir(path) == -1 ? errno : 0;

    int saved_errno = 0;
    if (chmod(path, 0755) != 0) {
        saved_errno = errno;
    } else if (rmdir(path) != 0) {
        saved_errno = errno;
    }
    if (saved_errno != 0) {
        errno = saved_errno;
        return -saved_errno;
    }
    return result;
}

static void run_basic_test_cases(const struct basic_test_case *test_cases, size_t count)
{
    for (size_t i = 0; i < count; i++) {
        const struct basic_test_case *tc = &test_cases[i];

        errno = 0;
        int r = chdir(tc->name);
        if (tc->exp_ret == 0) {
            CHECK_RET(r, 0, tc->desc);
            char cwd[512];
            CHECK(getcwd(cwd, sizeof(cwd)) != NULL, "chdir: getcwd after chdir");
            CHECK(strcmp(cwd, tc->exp_cwd) == 0, "chdir: cwd equals expected");
            CHECK_RET(chdir(PATH_FAMILY_BASE), 0, "chdir: reset to BASE");
        } else {
            CHECK(r == -1 && errno == tc->exp_errno, tc->desc);
        }
    }
}

static void run_eacces_test_cases(const struct eacces_test_case *test_cases, size_t count)
{
    for (size_t i = 0; i < count; i++) {
        const struct eacces_test_case *tc = &test_cases[i];
        int probe_value = 0;
        int probe_status = path_run_as_dropped_user(&probe_value, probe_chdir_eacces, (void *)tc);
        CHECK(probe_status >= 0, "chdir: launch dropped-user EACCES probe");
        if (probe_status == PATH_DROP_PROBE_SETUID_FAILED) {
            CHECK(0, "chdir: setuid failed in child for EACCES probe");
        } else if (probe_status == PATH_DROP_PROBE_OK) {
            if (probe_value != tc->exp_errno) {
                errno = probe_value > 0 ? probe_value : -probe_value;
            }
            CHECK(probe_value == tc->exp_errno, tc->desc);
        }
    }
}

void test_chdir(void)
{
    char old_cwd[512];
    CHECK(getcwd(old_cwd, sizeof(old_cwd)) != NULL, "chdir: capture old cwd");

    CHECK_RET(chdir(PATH_FAMILY_BASE), 0, "chdir: chdir(BASE)");

    char target_name[] = "chdir_target";
    char drop_parent[] = "chdir_drop_parent";
    mkdir(target_name, 0755);

    char expect_target[512];
    snprintf(expect_target, sizeof(expect_target), "%s/%s", PATH_FAMILY_BASE, target_name);

    char base_parent[512];
    snprintf(base_parent, sizeof(base_parent), "%s", PATH_FAMILY_BASE);
    char *slash = strrchr(base_parent, '/');
    if (slash && slash != base_parent) {
        *slash = '\0';
    } else {
        strcpy(base_parent, "/");
    }

    mkdir("symloop", 0755);
    symlink("../symloop", "symloop/symloop");
    char loop_path[1024];
    strcpy(loop_path, "symloop");
    for (int i = 0; i < 45; i++) {
        strcat(loop_path, "/symloop");
    }

    char regfile[256];
    path_join(regfile, sizeof(regfile), "regfile");

    char too_long_name[NAME_MAX + 32];
    memset(too_long_name, 'a', sizeof(too_long_name) - 1);
    too_long_name[sizeof(too_long_name) - 1] = '\0';

    struct path_perm_matrix_entry perm_entries[] = {
        {drop_parent, 0777, PATH_PERM_DIR},
    };
    path_cleanup_perm_matrix_at(AT_FDCWD, perm_entries, sizeof(perm_entries) / sizeof(perm_entries[0]));
    CHECK_RET(path_setup_perm_matrix_at(AT_FDCWD, perm_entries, sizeof(perm_entries) / sizeof(perm_entries[0])),
              0,
              "chdir: setup permission matrix");

    char eacces_parent[512];
    snprintf(eacces_parent, sizeof(eacces_parent), "%s/%s", PATH_FAMILY_BASE, drop_parent);

    struct basic_test_case test_cases[] = {
        /* (a) 进入存在目录：cwd 应变为 BASE/target */
        {target_name, 0, 0, expect_target, "chdir: to existing directory"},
        /* (b) "."：cwd 不变 */
        {".", 0, 0, PATH_FAMILY_BASE, "chdir: '.'"},
        /* (c) ".."：cwd 变为 BASE 的父目录 */
        {"..", 0, 0, base_parent, "chdir: '..'"},
        /* (d) "/"：cwd 变为根目录 */
        {"/", 0, 0, "/", "chdir: '/'"},
        /* (e) 不存在路径：ENOENT */
        {"/tmp/starry_syscall_test_path_family/no_such_path", -1, ENOENT, NULL, "chdir: missing path -> ENOENT"},
        /* (f) 普通文件：ENOTDIR */
        {regfile, -1, ENOTDIR, NULL, "chdir: regfile -> ENOTDIR"},
        /* (g) 符号链接循环/过深：ELOOP */
        {loop_path, -1, ELOOP, NULL, "chdir: symlink loop -> ELOOP"},
        /* (i) 单路径分量超长：ENAMETOOLONG */
        {too_long_name, -1, ENAMETOOLONG, NULL, "chdir: component too long -> ENAMETOOLONG"},
    };
    run_basic_test_cases(test_cases, sizeof(test_cases) / sizeof(test_cases[0]));

    if (geteuid() == 0) {
        struct eacces_test_case eacces_cases[] = {
            {
                .parent_dir = eacces_parent,
                .dir_name = "chdir_noexec_dir",
                .mode = 0666,
                .exp_errno = EACCES,
                .desc = "chdir: directory without execute permission -> EACCES",
            },
        };
        run_eacces_test_cases(eacces_cases, sizeof(eacces_cases) / sizeof(eacces_cases[0]));
    } else {
        printf("  SKIP | %s:%d | chdir: needs_root=1 for permission coverage\n",
               __FILE__,
               __LINE__);
    }

    path_cleanup_perm_matrix_at(AT_FDCWD, perm_entries, sizeof(perm_entries) / sizeof(perm_entries[0]));
    CHECK_RET(chdir(old_cwd), 0, "chdir: restore old cwd");
}
