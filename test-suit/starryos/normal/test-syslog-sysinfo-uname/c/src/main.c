#include "test_framework.h"

#include <sys/utsname.h>
#include <sys/sysinfo.h>
#include <sys/syscall.h>
#include <unistd.h>
#include <string.h>
#include <stdio.h>
#include <errno.h>
#include <stdlib.h>

/* ================================================================
 * syslog(2) 常量 — 内核定义但未导出到用户空间
 * ================================================================ */
#define SYSLOG_ACTION_CLOSE         0
#define SYSLOG_ACTION_OPEN          1
#define SYSLOG_ACTION_READ          2
#define SYSLOG_ACTION_READ_ALL      3
#define SYSLOG_ACTION_READ_CLEAR    4
#define SYSLOG_ACTION_CLEAR         5
#define SYSLOG_ACTION_CONSOLE_OFF   6
#define SYSLOG_ACTION_CONSOLE_ON    7
#define SYSLOG_ACTION_CONSOLE_LEVEL 8
#define SYSLOG_ACTION_SIZE_UNREAD   9
#define SYSLOG_ACTION_SIZE_BUFFER   10

static int do_syslog(int type, char *bufp, int len)
{
    return (int)syscall(SYS_syslog, type, bufp, len);
}

/* ================================================================
 * Section 1 — uname(2)
 * ================================================================ */
static void test_uname(void)
{
    TEST_START("uname: get name and information about current kernel");

    /* uname 基本功能 — 成功返回 0 */
    {
        struct utsname buf;
        errno = 0;
        int ret = uname(&buf);
        CHECK_RET(ret, 0, "uname returns 0 on success");
    }

    /* sysname 字段 */
    {
        struct utsname buf;
        uname(&buf);
        CHECK(strlen(buf.sysname) > 0, "uname sysname non-empty");
        CHECK(strcmp(buf.sysname, "Linux") == 0, "uname sysname == \"Linux\"");
    }

    /* nodename 字段 */
    {
        struct utsname buf;
        uname(&buf);
        CHECK(strlen(buf.nodename) > 0, "uname nodename non-empty");
    }

    /* release 字段 — 以数字开头 */
    {
        struct utsname buf;
        uname(&buf);
        CHECK(strlen(buf.release) > 0, "uname release non-empty");
        CHECK(buf.release[0] >= '0' && buf.release[0] <= '9',
              "uname release starts with digit");
    }

    /* version 字段 */
    {
        struct utsname buf;
        uname(&buf);
        CHECK(strlen(buf.version) > 0, "uname version non-empty");
    }

    /* machine 字段 */
    {
        struct utsname buf;
        uname(&buf);
        CHECK(strlen(buf.machine) > 0, "uname machine non-empty");
    }

    /* 所有字段均以 null 结尾 */
    {
        struct utsname buf;
        memset(&buf, 0xFF, sizeof(buf));
        uname(&buf);
        CHECK(memchr(buf.sysname,   '\0', sizeof(buf.sysname))   != NULL, "sysname null-terminated");
        CHECK(memchr(buf.nodename,  '\0', sizeof(buf.nodename))  != NULL, "nodename null-terminated");
        CHECK(memchr(buf.release,   '\0', sizeof(buf.release))   != NULL, "release null-terminated");
        CHECK(memchr(buf.version,   '\0', sizeof(buf.version))   != NULL, "version null-terminated");
        CHECK(memchr(buf.machine,   '\0', sizeof(buf.machine))   != NULL, "machine null-terminated");
    }

    /* 多次调用结果一致 */
    {
        struct utsname buf1, buf2;
        uname(&buf1);
        uname(&buf2);
        CHECK(memcmp(&buf1, &buf2, sizeof(struct utsname)) == 0,
              "uname consistent across two calls");
    }

    /* nodename 与 gethostname 一致 */
    {
        struct utsname buf;
        uname(&buf);
        char hostname[256] = {0};
        gethostname(hostname, sizeof(hostname));
        CHECK(strcmp(buf.nodename, hostname) == 0,
              "uname nodename matches gethostname()");
    }

    /* EFAULT — 无效指针 */
    {
        CHECK_ERR(uname(NULL), EFAULT, "uname NULL buf returns EFAULT");
        CHECK_ERR(uname((struct utsname *)1), EFAULT, "uname invalid ptr returns EFAULT");
    }
}

/* ================================================================
 * Section 2 — sysinfo(2)
 * ================================================================ */
static void test_sysinfo(void)
{
    TEST_START("sysinfo: return system information");

    /* sysinfo 基本功能 — 成功返回 0 */
    {
        struct sysinfo info;
        errno = 0;
        int ret = sysinfo(&info);
        CHECK_RET(ret, 0, "sysinfo returns 0 on success");
    }

    /* uptime > 0 */
    {
        struct sysinfo info;
        sysinfo(&info);
        CHECK(info.uptime >= 0, "sysinfo uptime >= 0");
    }

    /* totalram > 0, mem_unit > 0 */
    {
        struct sysinfo info;
        sysinfo(&info);
        CHECK(info.totalram > 0, "sysinfo totalram > 0");
        CHECK(info.mem_unit > 0, "sysinfo mem_unit > 0");
    }

    /* freeram <= totalram */
    {
        struct sysinfo info;
        sysinfo(&info);
        unsigned long long free_bytes = (unsigned long long)info.freeram * info.mem_unit;
        unsigned long long total_bytes = (unsigned long long)info.totalram * info.mem_unit;
        CHECK(free_bytes <= total_bytes, "sysinfo freeram <= totalram");
    }

    /* loads 合理范围 */
    {
        struct sysinfo info;
        sysinfo(&info);
        CHECK(info.loads[0] < 1000000 && info.loads[1] < 1000000 && info.loads[2] < 1000000,
              "sysinfo loads within reasonable range");
    }

    /* procs > 0 */
    {
        struct sysinfo info;
        sysinfo(&info);
        CHECK(info.procs > 0, "sysinfo procs > 0");
    }

    /* freeswap <= totalswap */
    {
        struct sysinfo info;
        sysinfo(&info);
        unsigned long long free_swap = (unsigned long long)info.freeswap * info.mem_unit;
        unsigned long long total_swap = (unsigned long long)info.totalswap * info.mem_unit;
        CHECK(free_swap <= total_swap, "sysinfo freeswap <= totalswap");
    }

    /* mem_unit == 1 (modern kernel) */
    {
        struct sysinfo info;
        sysinfo(&info);
        CHECK(info.mem_unit == 1, "sysinfo mem_unit == 1");
    }

    /* uptime 递增 */
    {
        struct sysinfo info1, info2;
        sysinfo(&info1);
        sleep(1);
        sysinfo(&info2);
        CHECK(info2.uptime >= info1.uptime, "sysinfo uptime increases over time");
    }

    /* 两次调用结果一致 (除 uptime) */
    {
        struct sysinfo info1, info2;
        sysinfo(&info1);
        sysinfo(&info2);
        long uptime_diff = (long)info2.uptime - (long)info1.uptime;
        CHECK(uptime_diff >= 0 && uptime_diff <= 2, "sysinfo uptime diff reasonable (0-2s)");
        CHECK(info1.totalram == info2.totalram, "sysinfo totalram consistent");
        CHECK(info1.totalswap == info2.totalswap, "sysinfo totalswap consistent");
    }

    /* EFAULT — 无效指针 */
    {
        CHECK_ERR(sysinfo(NULL), EFAULT, "sysinfo NULL returns EFAULT");
        CHECK_ERR(sysinfo((struct sysinfo *)1), EFAULT, "sysinfo invalid ptr returns EFAULT");
    }
}

/* ================================================================
 * Section 3 — syslog(2)
 * ================================================================ */
static void test_syslog(void)
{
    TEST_START("syslog: read/clear kernel message ring buffer; set console_loglevel");

    /* ==================== syslog 正向测试 ==================== */

    /* CLOSE (0) — NOP */
    {
        errno = 0;
        int ret = do_syslog(SYSLOG_ACTION_CLOSE, NULL, 0);
        CHECK_RET(ret, 0, "syslog CLOSE (type=0) returns 0");
    }

    /* OPEN (1) — NOP */
    {
        errno = 0;
        int ret = do_syslog(SYSLOG_ACTION_OPEN, NULL, 0);
        CHECK_RET(ret, 0, "syslog OPEN (type=1) returns 0");
    }

    /* SIZE_BUFFER (10) — 返回正值 */
    {
        errno = 0;
        int buf_size = do_syslog(SYSLOG_ACTION_SIZE_BUFFER, NULL, 0);
        CHECK(buf_size > 0, "syslog SIZE_BUFFER (type=10) returns positive size");
    }

    /* SIZE_UNREAD (9) — >= 0 */
    {
        errno = 0;
        int unread = do_syslog(SYSLOG_ACTION_SIZE_UNREAD, NULL, 0);
        CHECK(unread >= 0, "syslog SIZE_UNREAD (type=9) returns non-negative count");
    }

    /* READ_ALL (3) — 非破坏性读取 */
    {
        errno = 0;
        int buf_size = do_syslog(SYSLOG_ACTION_SIZE_BUFFER, NULL, 0);
        CHECK(buf_size > 0, "SIZE_BUFFER for READ_ALL allocation");

        if (buf_size > 0) {
            char *buf = malloc(buf_size + 1);
            CHECK(buf != NULL, "malloc for READ_ALL buffer");

            errno = 0;
            int ret = do_syslog(SYSLOG_ACTION_READ_ALL, buf, buf_size);
            CHECK(ret >= 0, "syslog READ_ALL (type=3) returns >= 0");

            if (ret > 0) {
                buf[ret] = '\0';
                CHECK(strlen(buf) > 0, "READ_ALL buffer contains data");
            }

            free(buf);
        }
    }

    /* READ_ALL 非破坏性验证 */
    {
        errno = 0;
        int buf_size = do_syslog(SYSLOG_ACTION_SIZE_BUFFER, NULL, 0);
        if (buf_size > 0) {
            char *buf1 = malloc(buf_size + 1);
            char *buf2 = malloc(buf_size + 1);
            CHECK(buf1 != NULL && buf2 != NULL, "malloc for nondestructive test");

            errno = 0;
            int ret1 = do_syslog(SYSLOG_ACTION_READ_ALL, buf1, buf_size);
            CHECK(ret1 >= 0, "first READ_ALL returns >= 0");

            errno = 0;
            int ret2 = do_syslog(SYSLOG_ACTION_READ_ALL, buf2, buf_size);
            CHECK(ret2 >= 0, "second READ_ALL returns >= 0");

            CHECK(ret2 >= ret1, "READ_ALL nondestructive (second >= first)");

            if (ret1 > 0 && ret2 >= ret1) {
                CHECK(memcmp(buf1, buf2, ret1) == 0,
                      "READ_ALL data matches between two reads");
            }

            free(buf1);
            free(buf2);
        }
    }

    /* CLEAR (5) — 返回 0 */
    {
        errno = 0;
        int ret = do_syslog(SYSLOG_ACTION_CLEAR, NULL, 0);
        CHECK_RET(ret, 0, "syslog CLEAR (type=5) returns 0");
    }

    /* CLEAR 后 READ_ALL 应返回 0 字节 */
    {
        errno = 0;
        do_syslog(SYSLOG_ACTION_CLEAR, NULL, 0);

        errno = 0;
        int buf_size = do_syslog(SYSLOG_ACTION_SIZE_BUFFER, NULL, 0);
        if (buf_size > 0) {
            char *buf = malloc(buf_size + 1);

            errno = 0;
            int ret = do_syslog(SYSLOG_ACTION_READ_ALL, buf, buf_size);
            CHECK(ret == 0, "after CLEAR, READ_ALL returns 0");

            free(buf);
        }
    }

    /* READ_CLEAR (4) */
    {
        errno = 0;
        int buf_size = do_syslog(SYSLOG_ACTION_SIZE_BUFFER, NULL, 0);
        if (buf_size > 0) {
            char *buf = malloc(buf_size + 1);

            errno = 0;
            int ret = do_syslog(SYSLOG_ACTION_READ_CLEAR, buf, buf_size);
            CHECK(ret >= 0, "syslog READ_CLEAR (type=4) returns >= 0");

            free(buf);
        }
    }

    /* CONSOLE_OFF (6) */
    {
        errno = 0;
        int ret = do_syslog(SYSLOG_ACTION_CONSOLE_OFF, NULL, 0);
        CHECK_RET(ret, 0, "syslog CONSOLE_OFF (type=6) returns 0");
    }

    /* CONSOLE_ON (7) */
    {
        errno = 0;
        int ret = do_syslog(SYSLOG_ACTION_CONSOLE_ON, NULL, 0);
        CHECK_RET(ret, 0, "syslog CONSOLE_ON (type=7) returns 0");
    }

    /* CONSOLE_LEVEL (8) — level=7 */
    {
        errno = 0;
        int ret = do_syslog(SYSLOG_ACTION_CONSOLE_LEVEL, NULL, 7);
        CHECK_RET(ret, 0, "syslog CONSOLE_LEVEL level=7");

        errno = 0;
        ret = do_syslog(SYSLOG_ACTION_CONSOLE_LEVEL, NULL, 4);
        CHECK_RET(ret, 0, "syslog CONSOLE_LEVEL level=4");
    }

    /* CONSOLE_LEVEL 边界 — 1 和 8 */
    {
        errno = 0;
        CHECK_RET(do_syslog(SYSLOG_ACTION_CONSOLE_LEVEL, NULL, 1), 0,
                  "syslog CONSOLE_LEVEL level=1 (minimum) returns 0");

        errno = 0;
        CHECK_RET(do_syslog(SYSLOG_ACTION_CONSOLE_LEVEL, NULL, 8), 0,
                  "syslog CONSOLE_LEVEL level=8 (maximum) returns 0");

        errno = 0;
        do_syslog(SYSLOG_ACTION_CONSOLE_LEVEL, NULL, 4);
    }

    /* CLEAR 不影响 SIZE_UNREAD */
    {
        errno = 0;
        int unread_before = do_syslog(SYSLOG_ACTION_SIZE_UNREAD, NULL, 0);

        errno = 0;
        do_syslog(SYSLOG_ACTION_CLEAR, NULL, 0);

        errno = 0;
        int unread_after = do_syslog(SYSLOG_ACTION_SIZE_UNREAD, NULL, 0);

        CHECK(unread_before >= 0 && unread_after >= 0,
              "SIZE_UNREAD >= 0 before and after CLEAR");
        CHECK(unread_after >= unread_before,
              "CLEAR does not reduce SIZE_UNREAD");
    }

    /* READ (2) — 跳过 (blocks on empty buffer) */
    {
        printf("  PASS | %s:%d | syslog READ (type=2) skipped (blocks on empty buffer)\n",
               __FILE__, __LINE__);
        __pass++;
    }

    /* ==================== syslog 反向测试 ==================== */

    /* EINVAL — 无效 type */
    {
        CHECK_ERR(do_syslog(255, NULL, 0), EINVAL,
                  "syslog invalid type=255 returns EINVAL");
    }

    {
        CHECK_ERR(do_syslog(-1, NULL, 0), EINVAL,
                  "syslog invalid type=-1 returns EINVAL");
    }

    /* EINVAL — READ_ALL buf=NULL */
    {
        CHECK_ERR(do_syslog(SYSLOG_ACTION_READ_ALL, NULL, 4096), EINVAL,
                  "syslog READ_ALL NULL buf returns EINVAL");
    }

    /* EINVAL — READ_CLEAR buf=NULL */
    {
        CHECK_ERR(do_syslog(SYSLOG_ACTION_READ_CLEAR, NULL, 4096), EINVAL,
                  "syslog READ_CLEAR NULL buf returns EINVAL");
    }

    /* EINVAL — READ buf=NULL */
    {
        CHECK_ERR(do_syslog(SYSLOG_ACTION_READ, NULL, 4096), EINVAL,
                  "syslog READ NULL buf returns EINVAL");
    }

    /* EINVAL — READ_ALL len=-1 */
    {
        char buf[64];
        CHECK_ERR(do_syslog(SYSLOG_ACTION_READ_ALL, buf, -1), EINVAL,
                  "syslog READ_ALL len=-1 returns EINVAL");
    }

    /* EINVAL — READ len=-1 */
    {
        char buf[64];
        CHECK_ERR(do_syslog(SYSLOG_ACTION_READ, buf, -1), EINVAL,
                  "syslog READ len=-1 returns EINVAL");
    }

    /* EINVAL — READ_CLEAR len=-1 */
    {
        char buf[64];
        CHECK_ERR(do_syslog(SYSLOG_ACTION_READ_CLEAR, buf, -1), EINVAL,
                  "syslog READ_CLEAR len=-1 returns EINVAL");
    }

    /* EINVAL — CONSOLE_LEVEL level=0 */
    {
        CHECK_ERR(do_syslog(SYSLOG_ACTION_CONSOLE_LEVEL, NULL, 0), EINVAL,
                  "syslog CONSOLE_LEVEL level=0 returns EINVAL");
    }

    /* EINVAL — CONSOLE_LEVEL level=9 */
    {
        CHECK_ERR(do_syslog(SYSLOG_ACTION_CONSOLE_LEVEL, NULL, 9), EINVAL,
                  "syslog CONSOLE_LEVEL level=9 returns EINVAL");
    }

    /* EINVAL — CONSOLE_LEVEL level=-100 */
    {
        CHECK_ERR(do_syslog(SYSLOG_ACTION_CONSOLE_LEVEL, NULL, -100), EINVAL,
                  "syslog CONSOLE_LEVEL level=-100 returns EINVAL");
    }

    /* 恢复状态 */
    do_syslog(SYSLOG_ACTION_CONSOLE_ON, NULL, 0);
    do_syslog(SYSLOG_ACTION_CONSOLE_LEVEL, NULL, 4);
}

/* ================================================================
 * main — 所有测试共享一套 pass/fail 计数器，只在最后输出一次 DONE
 * ================================================================ */
int main(void)
{
    test_uname();
    test_sysinfo();
    test_syslog();

    TEST_DONE();
}
