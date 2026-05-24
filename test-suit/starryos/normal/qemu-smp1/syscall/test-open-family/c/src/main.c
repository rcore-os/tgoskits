#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"

#include <fcntl.h>
#include <stdio.h>
#include <sys/stat.h>
#include <unistd.h>

/*
 * test-open-family —— open / openat 系统调用族「地毯式全覆盖」测例汇总
 *
 * 设计：30 个模块文件按维度切分（16 阶段① 基础+矩阵含 stress / 8 阶段② ERRNO / 4 阶段③ openat / 2 阶段④ 补充：设备节点目标 + fd 偏移/定位 I/O）；每个模块自管私有 /tmp/topen_<name>/ 目录，
 * 自计 __pass/__fail，run() 末尾打印小结并 return __fail。main 按顺序跑、汇总。
 *
 * 模块 → 见 notes/02 §17 + notes/10 §1.2 矩阵设计表。
 *
 * 暴露 starry vs Linux 差异的 case 一律不进本套件，移到 bugfix/bug-*。
 *
 * 退出码：0 = 全 PASS；非 0 = 有 case 不达预期。
 */

/* ── 各模块 run() 函数（每个返回该模块的失败数）── */
/* 阶段① 基础 + 矩阵 */
int open_access_run(void);
int open_mode_umask_run(void);
int open_create_run(void);
int open_excl_run(void);
int open_trunc_run(void);
int open_append_run(void);
int open_directory_run(void);
int open_nofollow_run(void);
int open_cloexec_run(void);
int open_nonblock_run(void);
int open_path_run(void);
int open_flag_matrix_run(void);
int open_silent_flags_run(void);
int open_fd_semantics_run(void);
int open_creat_alias_run(void);
int open_stress_run(void);
/* 阶段② open ERRNO */
int open_err_efault_run(void);
int open_err_einval_run(void);
int open_err_enametoolong_run(void);
int open_err_eloop_run(void);
int open_err_eintr_run(void);
int open_err_enxio_run(void);
int open_err_etxtbsy_run(void);
int open_err_misc_run(void);
/* 阶段③ openat */
int openat_dirfd_run(void);
int openat_err_run(void);
int openat_flag_matrix_run(void);
int openat_creat_run(void);
/* 阶段④ 补充：未覆盖的 open 目标类型 / fd 偏移语义 */
int open_dev_null_zero_run(void);
int open_pread_pwrite_run(void);

/* ── global setup / teardown ──
 * 建立全模块共用的只读基线环境：OF_DIR 子树。 */
static void global_setup(void)
{
    /* 防御性清理可能的残留 */
    cleanup_tree(OF_DIR);

    umask(0);                                              /* 隔离 umask 对 mode 测试的影响 */

    CHECK(ensure_dir(OF_DIR) == 0,                         "global_setup: mkdir " OF_DIR);
    CHECK(write_file(OF_REGULAR, "hello", 5, 0644) == 0,   "global_setup: create regular with 'hello'");
    CHECK(ensure_dir(OF_SUBDIR) == 0,                      "global_setup: mkdir " OF_SUBDIR);
    CHECK(symlink(OF_REGULAR, OF_SYMLINK) == 0,            "global_setup: symlink_to_reg -> regular");
    CHECK(symlink(OF_DIR "/nope", OF_DANGLING) == 0,       "global_setup: dangling symlink");
    CHECK(symlink(OF_SUBDIR, OF_SYM2DIR) == 0,             "global_setup: symlink_to_dir -> subdir");
}

static void global_teardown(void)
{
    cleanup_tree(OF_DIR);
}

int main(void)
{
    TEST_START("open family: 地毯式全覆盖（30 模块）");

    global_setup();
    /* setup_fail：取自 global_setup 的 __fail（来自 CHECK 宏的累计）。
     *
     * 历史背景：v6/v7 上观察到 __fail 在 starry QEMU 内读出 6381921 这种 junk 值
     * 即使 global_setup 全 PASS（怀疑是跨 TU static 内存可见性问题）。早期版本
     * 用 setup_fail = 0 强制清零并 (void)__fail —— 但 codex review 指出这会让
     * setup 真失败时（如 OF_DIR 创建失败 / symlink 创建失败）silently 通过 CI，
     * 是 false-pass 风险。
     *
     * 修法：保留 __fail 读取，但把 junk 值（>1000）当 hard failure 而非 0：
     * - sane 范围（0..1000）：直接采纳（setup CHECK 数 ≤ 6 个，不可能更高）
     * - 异常范围（<0 或 ≥1000）：判定为「setup 检测异常」也是 fail，记 1 强制非 0
     * 这样无论 __fail 是真失败还是 junk，都不会误报全 PASS。 */
    int setup_fail;
    if (__fail >= 0 && __fail < 1000) {
        setup_fail = __fail;
    } else {
        printf("  setup_fail: <suspect __fail=%d, treat as 1 hard failure>\n", __fail);
        setup_fail = 1;
    }

    /* 单线收集：每个 run() 立即 sum + print，避免 totals[] 中间数组在 starry
     * QEMU 上观察到的诡异 corrupt（v6 上 totals[1] 读出 6381921 但 mode_umask
     * 自报 0 fail）。
     *
     * 修法（codex review）：suspect rc 不再当 0（false-pass 风险），改记 1 强制
     * 非 0 → 任何模块的 corrupted return 都让 total_fail > 0 让 CI 红。 */
    int total_fail = setup_fail;
    int rc;

    #define COLLECT(label, call) do {                                          \
        rc = (call);                                                            \
        if (rc < 0 || rc > 1000000) {                                           \
            printf("  %-15s : <suspect rc=%d, treat as 1 hard failure>\n", label, rc); \
            rc = 1;                                                             \
        } else {                                                                \
            printf("  %-15s : %d fail\n", label, rc);                           \
        }                                                                       \
        total_fail += rc;                                                       \
    } while (0)

    printf("================================================\n");
    printf("  setup: %d fail\n", setup_fail);

    /* 阶段① */
    COLLECT("access",       open_access_run());
    COLLECT("mode_umask",   open_mode_umask_run());
    COLLECT("create",       open_create_run());
    COLLECT("excl",         open_excl_run());
    COLLECT("trunc",        open_trunc_run());
    COLLECT("append",       open_append_run());
    COLLECT("directory",    open_directory_run());
    COLLECT("nofollow",     open_nofollow_run());
    COLLECT("cloexec",      open_cloexec_run());
    COLLECT("nonblock",     open_nonblock_run());
    COLLECT("path",         open_path_run());
    COLLECT("flag_matrix",  open_flag_matrix_run());
    COLLECT("silent_flags", open_silent_flags_run());
    COLLECT("fd_semantics", open_fd_semantics_run());
    COLLECT("creat_alias",  open_creat_alias_run());
    COLLECT("stress",       open_stress_run());
    /* 阶段② */
    COLLECT("err_efault",   open_err_efault_run());
    COLLECT("err_einval",   open_err_einval_run());
    COLLECT("err_namelen",  open_err_enametoolong_run());
    COLLECT("err_eloop",    open_err_eloop_run());
    COLLECT("err_eintr",    open_err_eintr_run());
    COLLECT("err_enxio",    open_err_enxio_run());
    COLLECT("err_etxtbsy",  open_err_etxtbsy_run());
    COLLECT("err_misc",     open_err_misc_run());
    /* 阶段③ */
    COLLECT("openat_dirfd", openat_dirfd_run());
    COLLECT("openat_err",   openat_err_run());
    COLLECT("openat_flagmat", openat_flag_matrix_run());
    COLLECT("openat_creat", openat_creat_run());
    /* 阶段④ 补充 */
    COLLECT("dev_null_zero", open_dev_null_zero_run());
    COLLECT("pread_pwrite",  open_pread_pwrite_run());

    #undef COLLECT

    global_teardown();

    printf("  -------------------------------------------\n");
    printf("  TOTAL: %d fail | RESULT: %s\n",
           total_fail, total_fail == 0 ? "ALL PASS" : "HAS FAILURES");
    printf("================================================\n\n");

    return total_fail > 0 ? 1 : 0;
}
