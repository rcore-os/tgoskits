#include "path_common.h"

/*
 * getcwd(2) — get current working directory.
 *
 * man 2 getcwd:
 *   "getcwd() copies an absolute pathname of the current working directory to
 *    the array pointed to by buf, which is of length size."
 *   "If the length of the absolute pathname of the current working directory,
 *    including the terminating null byte, exceeds size bytes, NULL is returned,
 *    and errno is set to ERANGE."
 *
 * 测试覆盖：
 *   (a) buf/size 合法 → 返回正长度（包含结尾 '\0'），内容为当前绝对路径
 *   (b) buf 指向非法地址 → -1 EFAULT
 *   (c) size 传入无效值（负数）→ -1 EFAULT
 *   (d) size=0 / size=1 → -1 ERANGE（长度不足优先于 buf 合法性）
 *   (e) buf=NULL 且 size=1 → -1 ERANGE（同上，因长度不足而失败）
 *   (f) raw vs libc：此处使用 syscall(SYS_getcwd) 避免不同 libc 对参数的
 *       预处理/扩展语义差异影响（例如 NULL/0 的分配语义等）
 */

static long raw_getcwd(void *buf, size_t size)
{
    return syscall(SYS_getcwd, buf, size);
}

void test_getcwd(void)
{
    char old_cwd[512];
    CHECK(getcwd(old_cwd, sizeof(old_cwd)) != NULL, "getcwd: capture old cwd");

    CHECK_RET(chdir(PATH_FAMILY_BASE), 0, "getcwd: chdir(BASE)");

    char cwd[512];
    size_t expected_len = strlen(PATH_FAMILY_BASE) + 1;

    char small[2];

    struct test_case {
        void *buf;
        size_t size;
        long exp_ret;
        int exp_errno;
    };

    struct test_case test_cases[] = {
        /* (a) 正常路径：buf/size 合法，返回长度包含 '\0' */
        {cwd, sizeof(cwd), 0, 0},
        /* (b) buf 非法地址：EFAULT */
        {(void *)-1, sizeof(cwd), -1, EFAULT},
        /* (c) size 无效（负数）：EFAULT */
        {NULL, (size_t)-1, -1, EFAULT},
        /* (d) size=0：ERANGE */
        {small, 0, -1, ERANGE},
        /* (d) size=1：ERANGE */
        {small, 1, -1, ERANGE},
        /* (e) buf=NULL 且 size=1：ERANGE（长度不足优先） */
        {NULL, 1, -1, ERANGE},
    };

    const char *case_msgs[] = {
        "getcwd: raw success",
        "getcwd: bad address -> EFAULT",
        "getcwd: invalid size (-1) -> EFAULT",
        "getcwd: size=0 -> ERANGE",
        "getcwd: size=1 -> ERANGE",
        "getcwd: NULL buf, size=1 -> ERANGE",
    };

    for (size_t i = 0; i < sizeof(test_cases) / sizeof(test_cases[0]); i++) {
        struct test_case *tc = &test_cases[i];
        const char *msg = case_msgs[i];

        errno = 0;
        long r = raw_getcwd(tc->buf, tc->size);

        if (tc->exp_ret == 0) {
            CHECK(r > 0, msg);
            CHECK((size_t)r == expected_len, "getcwd: raw return length includes trailing NUL");
            CHECK(strcmp(cwd, PATH_FAMILY_BASE) == 0, "getcwd: cwd equals BASE");
            CHECK(errno == 0, "getcwd: success keeps errno==0");
        } else {
            CHECK(r == -1 && errno == tc->exp_errno, msg);
        }
    }

    chdir(old_cwd);
}
