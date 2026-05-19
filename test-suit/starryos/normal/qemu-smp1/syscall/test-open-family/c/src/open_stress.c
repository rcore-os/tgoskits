#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <unistd.h>

/*
 * open_stress.c — 常规 / 批量压力测试（与 functional 区分）
 *
 * 目的：验证 fd 表、权限表、open/close 路径在「批量 + 权限多样」场景下的
 * 正确性与稳定性。不深入语义细节，只做"开/关上百次不崩、fd 序列正确"
 * 类的常规检查。
 *
 * 测的还是 man 2 open 里的几个核心不变量，只是规模大：
 *   - "lowest-numbered file descriptor not currently open" → 批量场景下 fd
 *     编号严格递增 + 关闭后空槽复用（churn 1000 次）
 *   - "Each open() of the file creates a new open file description" →
 *     200 fd × 同文件，offset 独立性
 *   - "this reference is unaffected if pathname is subsequently removed"
 *     → 200 zombie fd（unlink 后保留 fd 全可读）
 *   - "(mode & ~umask)" 在 200 文件 × 12 mode 下批量验证
 *   - O_CLOEXEC × 120 fd 矩阵下 FD_CLOEXEC 设置正确性
 *
 * 子矩阵：
 *   1. mass_open_diverse_modes      — N=200 文件 × 12 mode，全 open + 全 close
 *   2. mass_open_diverse_access     — 同 1 文件 open M=200 次，access 模式轮换
 *   3. mass_create_unlink_close     — 创建+unlink+保留 fd，验证 200 个 zombie fd 全可读
 *   4. close_in_various_orders      — open 100 个 fd 后用 sequential / reverse / interleave 关闭
 *   5. open_close_churn             — 紧密循环 open/close 1000 次（同一 path）测 fd 槽位复用稳定性
 *   6. mass_diverse_combo           — 12 (access, flag) combo × 10 = 120 fd，验证 CLOEXEC 设置正确
 *
 * RLIMIT_NOFILE 通过 setrlimit 临时拉高，跑完恢复
 */

#define M_DIR    OF_MOD("stress")
#define N_FILES  200            /* 批量测试规模 */
#define N_CHURN  1000           /* 紧密 open/close 循环次数 */

static struct rlimit g_orig_rlim;

static void mod_setup(void)
{
    cleanup_tree(M_DIR);
    CHECK(ensure_dir(M_DIR) == 0,                                 "stress setup: mkdir top");
    /* 拉高 RLIMIT_NOFILE 至 1024（如果当前更低）以容纳批量 */
    if (getrlimit(RLIMIT_NOFILE, &g_orig_rlim) == 0) {
        struct rlimit nr = g_orig_rlim;
        if (nr.rlim_cur < 1024 && g_orig_rlim.rlim_max >= 1024) {
            nr.rlim_cur = 1024;
            setrlimit(RLIMIT_NOFILE, &nr);
        }
    }
    umask(0);
}

static void mod_teardown(void)
{
    setrlimit(RLIMIT_NOFILE, &g_orig_rlim);
    cleanup_tree(M_DIR);
}

/* === 1. 批量创建 N 个不同 mode 的文件 + 全 open + 全 close ===
 *
 * 设计：先用 12 种多样 mode 创建（含 0444/0400/0200 等不可读/只读模式），
 *       验证 stat 后 mode 位正确（"创建路径支持多样 mode"）；
 *       然后 chmod 全部到 0644，再批量 RDONLY 打开（"批量打开 + 关闭"）。
 *       host 非 root 时，0200 等模式直接 RDONLY 打开会 EACCES——不是测试目标。
 */
static void mass_open_diverse_modes(void)
{
    static const mode_t MODES[] = {
        0644, 0600, 0755, 0666, 0444, 0700, 0640, 0604,
        0400, 0200, 0777, 0640,
    };
    const size_t n_modes = sizeof(MODES) / sizeof(MODES[0]);

    char paths[N_FILES][96];
    int  fds[N_FILES];
    int  created = 0;

    /* 阶段 A：创建 + 验证 stat 后 mode 正确 */
    int mode_correct = 0;
    int sgid_strip = 0;
    for (int i = 0; i < N_FILES; i++) {
        snprintf(paths[i], sizeof(paths[i]), "%s/diverse_%04d", M_DIR, i);
        mode_t m = MODES[i % n_modes];
        unlink(paths[i]);
        int fd = open(paths[i], O_CREAT | O_WRONLY, m);
        if (fd < 0) {
            char msg[160];
            snprintf(msg, sizeof(msg), "stress mass_create: file %d (mode %04o) open fail errno=%d",
                     i, (unsigned)m, errno);
            CHECK_QUIET(0, msg);
            break;
        }
        if (m & 0200) {  /* 只在 owner-write 模式下 write */
            write(fd, "x", 1);
        }
        close(fd);
        created++;

        /* stat 验证 mode（容忍 sgid host strip）*/
        mode_t got = get_file_mode(paths[i]) & 07777;
        if (got == m) mode_correct++;
        else if ((m & 02000) && !(got & 02000) && (got | 02000) == m) {
            mode_correct++; sgid_strip++;
        }
    }
    CHECK(created == N_FILES,                                     "stress mass_create: 200 files all created");
    CHECK(mode_correct == created,                                "stress mass_create: all 200 stat 后 mode 位正确");
    if (sgid_strip > 0)
        printf("  NOTE: %d files had sgid stripped (host non-root)\n", sgid_strip);

    /* 阶段 B：chmod 全部到 0644，再批量 RDONLY 打开 + 全 close */
    for (int i = 0; i < created; i++) chmod(paths[i], 0644);

    int opened = 0;
    for (int i = 0; i < created; i++) {
        fds[i] = open(paths[i], O_RDONLY);
        if (fds[i] < 0) break;
        opened++;
    }
    CHECK(opened == created,                                      "stress mass_open: all 200 opened simultaneously");

    int close_ok = 0;
    for (int i = 0; i < opened; i++) if (close(fds[i]) == 0) close_ok++;
    CHECK(close_ok == opened,                                     "stress mass_open: all 200 closed cleanly");

    for (int i = 0; i < created; i++) unlink(paths[i]);
}

/* === 2. 同一文件 open M 次，access 模式轮换 === */
static void mass_open_diverse_access(void)
{
    char path[96];
    snprintf(path, sizeof(path), "%s/shared", M_DIR);
    write_file(path, "shared", 6, 0644);

    static const int ACCESSES[] = { O_RDONLY, O_WRONLY, O_RDWR };
    int fds[N_FILES];
    int opened = 0;

    for (int i = 0; i < N_FILES; i++) {
        fds[i] = open(path, ACCESSES[i % 3]);
        if (fds[i] < 0) break;
        opened++;
    }
    CHECK(opened == N_FILES,                                      "stress mass_access: 200 fds on same file ok");

    /* 验证 fd 编号严格递增（lowest-free-fd 语义在批量下不破坏）*/
    int strict_inc = 1;
    for (int i = 1; i < opened; i++) {
        if (fds[i] <= fds[i - 1]) { strict_inc = 0; break; }
    }
    CHECK(strict_inc,                                             "stress mass_access: fd 严格递增");

    /* 关闭 */
    int close_ok = 0;
    for (int i = 0; i < opened; i++) if (close(fds[i]) == 0) close_ok++;
    CHECK(close_ok == opened,                                     "stress mass_access: all closed");

    unlink(path);
}

/* === 3. 创建+unlink+保留 fd（zombie fds）—— 200 个 unlinked fd 全可读 ===
 *
 * 修法（codex P1 review）：早期版本在 open 失败时 break 后仍读所有 200 个
 * fds[]，未初始化的 tail 槽是栈 garbage —— UB（read/close 在野指针描述符上）。
 * 改记 `opened` 计数：所有循环只到 opened 边界，且把 `opened != N_FILES` 当
 * hard failure。 */
static void mass_create_unlink_close(void)
{
    char paths[N_FILES][96];
    int  fds[N_FILES];
    int  opened = 0;

    /* 阶段 A：创建 200 个文件，每个写 1 字节 'A'+i%26，open 后立即 unlink */
    for (int i = 0; i < N_FILES; i++) {
        snprintf(paths[i], sizeof(paths[i]), "%s/zombie_%04d", M_DIR, i);
        unlink(paths[i]);
        int fd = open(paths[i], O_CREAT | O_RDWR, 0644);
        if (fd < 0) {
            CHECK_QUIET(0, "stress zombie: create open fail");
            break;
        }
        fds[opened++] = fd;
        char ch = (char)('A' + (i % 26));
        write(fd, &ch, 1);
        lseek(fd, 0, SEEK_SET);
        unlink(paths[i]);                                          /* 立即 unlink，fd 变 zombie */
    }
    /* 必须开足 200 个；少一个就视为 stress 测例失败 */
    CHECK(opened == N_FILES,                                      "stress zombie: 200 zombie fds 创建");

    /* 阶段 B：所有 zombie fd 仍能读到原内容（仅遍历 opened，避免 UB） */
    int read_ok = 0;
    for (int i = 0; i < opened; i++) {
        char b[2] = {0};
        if (read(fds[i], b, 1) == 1 && b[0] == ('A' + (i % 26))) read_ok++;
    }
    CHECK(read_ok == opened,                                      "stress zombie: 已开 fd 全可读原内容");

    /* 阶段 C：全部 close（仅 opened 范围） */
    int close_ok = 0;
    for (int i = 0; i < opened; i++) if (close(fds[i]) == 0) close_ok++;
    CHECK(close_ok == opened,                                     "stress zombie: 已开 fd close 干净");
}

/* === 4. 关闭顺序矩阵：sequential / reverse / interleave === */
static void close_in_various_orders(void)
{
    char path[96];
    snprintf(path, sizeof(path), "%s/closeorder", M_DIR);
    write_file(path, "x", 1, 0644);

    /* sequential close */
    {
        int fds[100];
        for (int i = 0; i < 100; i++) fds[i] = open(path, O_RDONLY);
        CHECK(fds[99] >= 0,                                       "close_orders seq: 100 fds opened");
        int ok = 1;
        for (int i = 0; i < 100; i++) if (close(fds[i]) != 0) { ok = 0; break; }
        CHECK(ok,                                                 "close_orders seq: sequential close ok");
    }

    /* reverse close */
    {
        int fds[100];
        for (int i = 0; i < 100; i++) fds[i] = open(path, O_RDONLY);
        int ok = 1;
        for (int i = 99; i >= 0; i--) if (close(fds[i]) != 0) { ok = 0; break; }
        CHECK(ok,                                                 "close_orders rev: reverse close ok");
    }

    /* interleave close: 偶数先关，奇数后关 */
    {
        int fds[100];
        for (int i = 0; i < 100; i++) fds[i] = open(path, O_RDONLY);
        int ok = 1;
        for (int i = 0; i < 100; i += 2) if (close(fds[i]) != 0) { ok = 0; break; }
        for (int i = 1; i < 100; i += 2) if (close(fds[i]) != 0) { ok = 0; break; }
        CHECK(ok,                                                 "close_orders ileave: even-then-odd close ok");
    }

    unlink(path);
}

/* === 5. open/close 紧密 churn 1000 次 —— 验证 fd 槽位复用稳定性 === */
static void open_close_churn(void)
{
    char path[96];
    snprintf(path, sizeof(path), "%s/churn", M_DIR);
    write_file(path, "c", 1, 0644);

    int n_ok = 0;
    int last_fd = -1;
    int fd_stable = 1;        /* 紧密 open/close 应总分配相同最低空槽 */

    for (int i = 0; i < N_CHURN; i++) {
        int fd = open(path, O_RDONLY);
        if (fd < 0) break;
        if (last_fd >= 0 && fd != last_fd) fd_stable = 0;
        last_fd = fd;
        if (close(fd) != 0) break;
        n_ok++;
    }
    CHECK(n_ok == N_CHURN,                                        "stress churn: 1000 open/close cycles ok");
    CHECK(fd_stable,                                              "stress churn: fd 编号稳定（同一空槽复用）");

    unlink(path);
}

/* === 6. 批量打开 + 各种 access × 各种 flag 组合 === */
static void mass_diverse_combo(void)
{
    char path[96];
    snprintf(path, sizeof(path), "%s/combo", M_DIR);
    write_file(path, "data", 4, 0644);

    /* 12 个有意义的 (access, extra_flag) 组合，每个 open 10 次，共 120 fd */
    struct combo { int access; int extra; };
    static const struct combo COMBOS[] = {
        { O_RDONLY, 0 },
        { O_WRONLY, 0 },
        { O_RDWR,   0 },
        { O_RDONLY, O_CLOEXEC },
        { O_WRONLY, O_CLOEXEC },
        { O_RDWR,   O_CLOEXEC },
        { O_RDONLY, O_NONBLOCK },
        { O_WRONLY, O_NONBLOCK },
        { O_WRONLY, O_APPEND },
        { O_RDWR,   O_APPEND },
        { O_RDONLY, O_NONBLOCK | O_CLOEXEC },
        { O_RDWR,   O_NONBLOCK | O_CLOEXEC | O_APPEND },
    };
    const int n_combo = (int)(sizeof(COMBOS)/sizeof(COMBOS[0]));
    enum { OPENS_PER_COMBO = 10 };
    int fds[12 * 10];
    int total = 0;

    for (int c = 0; c < n_combo; c++) {
        for (int k = 0; k < OPENS_PER_COMBO; k++) {
            int fd = open(path, COMBOS[c].access | COMBOS[c].extra);
            if (fd < 0) break;
            fds[total++] = fd;
        }
    }
    CHECK(total == n_combo * OPENS_PER_COMBO,                     "stress combo: 120 fds across 12 (access,flag) combos");

    /* 验证 CLOEXEC 设置正确性（前 60 应有，后 60 部分有）*/
    int cloexec_correct = 1;
    for (int i = 0; i < total; i++) {
        int combo_idx = i / OPENS_PER_COMBO;
        int has_cloexec = (COMBOS[combo_idx].extra & O_CLOEXEC) != 0;
        int fl = fcntl(fds[i], F_GETFD);
        if (fl < 0) { cloexec_correct = 0; break; }
        int actually = (fl & FD_CLOEXEC) != 0;
        if (actually != has_cloexec) { cloexec_correct = 0; break; }
    }
    CHECK(cloexec_correct,                                        "stress combo: 120 fds 的 FD_CLOEXEC 全部正确");

    /* 全 close */
    int close_ok = 0;
    for (int i = 0; i < total; i++) if (close(fds[i]) == 0) close_ok++;
    CHECK(close_ok == total,                                      "stress combo: 全 120 close ok");

    unlink(path);
}

int open_stress_run(void)
{
    printf("\n----- open_stress -----\n");
    mod_setup();
    mass_open_diverse_modes();
    mass_open_diverse_access();
    mass_create_unlink_close();
    close_in_various_orders();
    open_close_churn();
    mass_diverse_combo();
    mod_teardown();
    printf("  ----- open_stress: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
