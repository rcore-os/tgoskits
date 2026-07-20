#define _GNU_SOURCE
#include "test_framework.h"
#include <ctype.h>
#include <fcntl.h>
#include <sys/mman.h>
#include <unistd.h>

/*
 * /proc/vmstat 回归测试。
 *
 * 触发背景 (为什么写这个测例):
 *   node_exporter 的 vmstat collector 解析 /proc/vmstat 导出 node_vmstat_* 指标。starry 此前
 *   没有 /proc/vmstat, 该 collector 无数据可导出 -> monitor 栈缺一个采集源。内核现按 Linux
 *   mm/vmstat.c 布局导出 /proc/vmstat, 只暴露真实维护的计数器: pgfault (缺页处理累计计数,
 *   mm/access.rs) 与 nr_free_pages (全局分配器实时空闲页)。不造假字段。
 *
 * Linux /proc/vmstat 布局 (man 5 proc):
 *   每行一个 "name value" 对, value 为十进制无符号整数。
 *
 * 断言:
 *   1. /proc/vmstat 可打开且非空 (修复前不存在 -> 打不开)。
 *   2. 每一非空行都是 "<name> <digits>" 布局。
 *   3. pgfault 字段存在且为数字。
 *   4. nr_free_pages 字段存在且为数字。
 *   5. pgfault > 0 (自启动以来已服务缺页)。
 *   6. nr_free_pages > 0 (真实空闲 RAM 计量)。
 *   7. mmap 256 匿名页成功 (用于制造缺页)。
 *   8. pgfault 是活计数器: 首触 256 页后严格增大 (证明跟踪真实缺页事件, 非静态桩)。
 */

#define VMSTAT_PATH "/proc/vmstat"

/* 读取整个文件到 buf (NUL 结尾); 返回字节数或 -1。*/
static long slurp(const char *path, char *buf, size_t cap)
{
    int fd = open(path, O_RDONLY);
    if (fd < 0)
        return -1;
    size_t n = 0;
    while (n < cap - 1) {
        ssize_t r = read(fd, buf + n, cap - 1 - n);
        if (r < 0) {
            close(fd);
            return -1;
        }
        if (r == 0)
            break;
        n += (size_t)r;
    }
    close(fd);
    buf[n] = '\0';
    return (long)n;
}

/* 在行首匹配 "name value"; 命中且 value 为数字时经 *out 返回并返回 1。*/
static int field(const char *buf, const char *name, unsigned long long *out)
{
    size_t nl = strlen(name);
    for (const char *p = buf; p && *p;) {
        if (strncmp(p, name, nl) == 0 && p[nl] == ' ') {
            const char *v = p + nl + 1;
            while (*v == ' ')
                v++;
            if (!isdigit((unsigned char)*v))
                return 0;
            unsigned long long val = 0;
            for (; isdigit((unsigned char)*v); v++)
                val = val * 10 + (unsigned long long)(*v - '0');
            *out = val;
            return 1;
        }
        const char *nlp = strchr(p, '\n');
        p = nlp ? nlp + 1 : NULL;
    }
    return 0;
}

/* 每一非空行都是 "<非空名> <非空数字串>"。*/
static int all_lines_name_value(const char *buf)
{
    for (const char *p = buf; p && *p;) {
        const char *nlp = strchr(p, '\n');
        size_t len = nlp ? (size_t)(nlp - p) : strlen(p);
        if (len > 0) {
            const char *sp = memchr(p, ' ', len);
            if (!sp || sp == p)
                return 0;
            const char *v = sp + 1;
            while (v < p + len && *v == ' ')
                v++;
            if (v >= p + len || !isdigit((unsigned char)*v))
                return 0;
            for (; v < p + len; v++)
                if (!isdigit((unsigned char)*v))
                    return 0;
        }
        p = nlp ? nlp + 1 : NULL;
    }
    return 1;
}

int main(void)
{
    TEST_START("/proc/vmstat exposes real pgfault + nr_free_pages counters");
    char msg[192];
    static char buf[8192];

    long n = slurp(VMSTAT_PATH, buf, sizeof buf);
    CHECK(n > 0, "/proc/vmstat 可打开且非空 (修复前不存在)");
    if (n <= 0) {
        TEST_DONE(8);
    }

    CHECK(all_lines_name_value(buf), "每行均为 '<name> <digits>' (mm/vmstat.c 布局)");

    unsigned long long pgfault0 = 0, nrfree = 0;
    int have_pgfault = field(buf, "pgfault", &pgfault0);
    CHECK(have_pgfault, "pgfault 字段存在且为数字");
    int have_nrfree = field(buf, "nr_free_pages", &nrfree);
    CHECK(have_nrfree, "nr_free_pages 字段存在且为数字");

    snprintf(msg, sizeof msg, "pgfault > 0 (自启动已服务缺页; 实际 %llu)", pgfault0);
    CHECK(have_pgfault && pgfault0 > 0, msg);

    snprintf(msg, sizeof msg, "nr_free_pages > 0 (真实空闲 RAM 计量; 实际 %llu)", nrfree);
    CHECK(have_nrfree && nrfree > 0, msg);

    /* 制造新缺页: mmap 一批匿名页并逐页首触, 再重读。pgfault 必须严格增大, 证明计数器活
     * 跟踪真实缺页事件, 而非静态值。*/
    const size_t PAGES = 256, PGSZ = 4096;
    char *area = mmap(NULL, PAGES * PGSZ, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(area != MAP_FAILED, "mmap 256 匿名页用于制造缺页");
    if (area != MAP_FAILED)
        for (size_t i = 0; i < PAGES; i++)
            area[i * PGSZ] = (char)i; /* 首触 -> 每页一次按需缺页 */

    unsigned long long pgfault1 = 0;
    n = slurp(VMSTAT_PATH, buf, sizeof buf);
    (void)field(buf, "pgfault", &pgfault1);
    snprintf(msg, sizeof msg, "pgfault 活计数: 触 256 页后增大 (%llu -> %llu)", pgfault0, pgfault1);
    CHECK(pgfault1 > pgfault0, msg);

    if (area != MAP_FAILED)
        munmap(area, PAGES * PGSZ);

    // 8 = 本文件 CHECK 总数。
    TEST_DONE(8);
}
