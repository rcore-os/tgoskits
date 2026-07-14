#define _GNU_SOURCE
#include "test_framework.h"
#include <ctype.h>

/*
 * /proc/<pid>/mountinfo 回归测试。
 *
 * 触发背景 (为什么写这个测例):
 *   glances / psutil 优先解析 /proc/pid/mountinfo (而非 /proc/mounts) 来发现挂载点,
 *   再对每个挂载点 statfs() 填 FILE SYS 区块。starry 此前没有 /proc/pid/mountinfo,
 *   glances FILE SYS 区块无法渲染。内核现按 Linux fs/proc_namespace.c show_mountinfo
 *   布局导出该文件。
 *
 * Linux show_mountinfo 每行布局 (man 5 proc §/proc/[pid]/mountinfo):
 *   id parent major:minor root mount_point mount_options [optional...] - fstype source super_opts
 *   "-" 是可选字段的终止符, 其后紧跟 fstype / mount source / super options。
 *
 * 断言:
 *   1. /proc/self/mountinfo 可打开 (修复前不存在 -> 打不开)。
 *   2. 文件非空。
 *   3-13. 根挂载点 "/" 的行满足 show_mountinfo 布局的每个字段约束。
 *   14. 伪文件系统挂载可见 (/proc, fstype proc)。
 */

#define MOUNTINFO_PATH "/proc/self/mountinfo"

typedef struct {
    int have_line; /* 找到 mount_point == "/" 的行 */
    int ntok_before; /* 分隔符 "-" 之前的令牌数 */
    int have_sep;
    char id[32];
    char parent[32];
    char majmin[32];
    char root[64];
    char opts[160];
    char fstype[48];
    char source[160];
    char super_opts[160];
} rootline_t;

static int all_digits(const char *s)
{
    if (!s || !*s)
        return 0;
    for (; *s; s++)
        if (!isdigit((unsigned char)*s))
            return 0;
    return 1;
}

/* 形如 "N:N", 冒号两侧均为非空数字串。*/
static int valid_majmin(const char *s)
{
    const char *colon = strchr(s, ':');
    if (!colon || colon == s || colon[1] == '\0')
        return 0;
    for (const char *p = s; p < colon; p++)
        if (!isdigit((unsigned char)*p))
            return 0;
    for (const char *p = colon + 1; *p; p++)
        if (!isdigit((unsigned char)*p))
            return 0;
    return 1;
}

static void copy_tok(char *dst, size_t cap, const char *src)
{
    if (!src) {
        dst[0] = '\0';
        return;
    }
    snprintf(dst, cap, "%s", src);
}

int main(void)
{
    TEST_START("/proc/self/mountinfo show_mountinfo layout");

    rootline_t rl;
    memset(&rl, 0, sizeof rl);
    int any_line = 0;
    int proc_found = 0;

    FILE *f = fopen(MOUNTINFO_PATH, "r");
    if (f) {
        char line[1024];
        while (fgets(line, sizeof line, f)) {
            char *toks[64];
            int n = 0;
            for (char *p = strtok(line, " \t\n"); p && n < 64;
                 p = strtok(NULL, " \t\n"))
                toks[n++] = p;
            if (n == 0)
                continue;
            any_line = 1;

            /* 定位可选字段终止符 "-"。*/
            int sep = -1;
            for (int i = 0; i < n; i++)
                if (strcmp(toks[i], "-") == 0) {
                    sep = i;
                    break;
                }

            const char *mp = n > 4 ? toks[4] : NULL;

            /* /proc 伪文件系统: 挂载点 /proc 且分隔符后 fstype == proc。*/
            if (mp && strcmp(mp, "/proc") == 0 && sep >= 0 && sep + 1 < n &&
                strcmp(toks[sep + 1], "proc") == 0)
                proc_found = 1;

            if (mp && strcmp(mp, "/") == 0 && !rl.have_line) {
                rl.have_line = 1;
                rl.ntok_before = sep >= 0 ? sep : n;
                rl.have_sep = sep >= 0;
                if (n > 0)
                    copy_tok(rl.id, sizeof rl.id, toks[0]);
                if (n > 1)
                    copy_tok(rl.parent, sizeof rl.parent, toks[1]);
                if (n > 2)
                    copy_tok(rl.majmin, sizeof rl.majmin, toks[2]);
                if (n > 3)
                    copy_tok(rl.root, sizeof rl.root, toks[3]);
                if (n > 5)
                    copy_tok(rl.opts, sizeof rl.opts, toks[5]);
                if (sep >= 0 && sep + 1 < n)
                    copy_tok(rl.fstype, sizeof rl.fstype, toks[sep + 1]);
                if (sep >= 0 && sep + 2 < n)
                    copy_tok(rl.source, sizeof rl.source, toks[sep + 2]);
                if (sep >= 0 && sep + 3 < n)
                    copy_tok(rl.super_opts, sizeof rl.super_opts, toks[sep + 3]);
            }
        }
        fclose(f);
    }

    char msg[256];

    CHECK(f != NULL, MOUNTINFO_PATH " 可打开 (修复前不存在)");
    CHECK(any_line, "mountinfo 至少一行");
    CHECK(rl.have_line, "含根挂载点 \"/\" 的行");

    snprintf(msg, sizeof msg,
             "根行分隔符前 >=6 字段 id/parent/maj:min/root/mp/opts (实际 %d)",
             rl.ntok_before);
    CHECK(rl.ntok_before >= 6, msg);

    snprintf(msg, sizeof msg, "字段1 mount id 为数字 (\"%s\")", rl.id);
    CHECK(all_digits(rl.id), msg);

    snprintf(msg, sizeof msg, "字段2 parent id 为数字 (\"%s\")", rl.parent);
    CHECK(all_digits(rl.parent), msg);

    snprintf(msg, sizeof msg, "字段3 major:minor 形如 N:N (\"%s\")", rl.majmin);
    CHECK(valid_majmin(rl.majmin), msg);

    snprintf(msg, sizeof msg, "字段4 fs 内 root == \"/\" (\"%s\")", rl.root);
    CHECK(strcmp(rl.root, "/") == 0, msg);

    snprintf(msg, sizeof msg, "字段6 mount options 含 rw (\"%s\")", rl.opts);
    CHECK(strstr(rl.opts, "rw") != NULL, msg);

    CHECK(rl.have_sep, "根行含 \"-\" 可选字段分隔符");

    snprintf(msg, sizeof msg, "分隔符后 fstype 非空 (\"%s\")", rl.fstype);
    CHECK(rl.fstype[0] != '\0', msg);

    snprintf(msg, sizeof msg, "分隔符后 mount source 非空 (\"%s\")", rl.source);
    CHECK(rl.source[0] != '\0', msg);

    snprintf(msg, sizeof msg, "分隔符后 super options 非空 (\"%s\")", rl.super_opts);
    CHECK(rl.super_opts[0] != '\0', msg);

    CHECK(proc_found, "含 /proc (fstype proc) 伪文件系统挂载行");

    // 14 = 本文件 CHECK 总数。
    TEST_DONE(14);
}
