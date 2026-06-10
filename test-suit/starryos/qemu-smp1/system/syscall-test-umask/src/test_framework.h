#pragma once

#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int __pass = 0;

static void __test_assert(int cond, const char *expr, const char *file,
                          int line)
{
    if (!cond) {
        printf("  ASSERTION FAILED | %s:%d | %s\n", file, line, expr);
        abort();
    }
}

#define ASSERT_ACTIVE(cond) __test_assert(!!(cond), #cond, __FILE__, __LINE__)

#define TEST_START(name)                                                \
    do {                                                                \
        printf("================================================\n");   \
        printf("  TEST: %s\n", name);                                  \
        printf("  FILE: %s\n", __FILE__);                              \
        printf("================================================\n");   \
    } while (0)

#define ASSERT_OK(call, msg)                                            \
    do {                                                                \
        errno = 0;                                                      \
        long __ret = (long)(call);                                      \
        int __err = errno;                                              \
        printf("  CHECK | %s | ret=%ld errno=%d (%s)\n",               \
               msg, __ret, __err, strerror(__err));                    \
        ASSERT_ACTIVE(__ret == 0);                                      \
        __pass++;                                                       \
    } while (0)

#define ASSERT_ERR(call, exp_errno, msg)                                \
    do {                                                                \
        errno = 0;                                                      \
        long __ret = (long)(call);                                      \
        int __err = errno;                                              \
        printf("  CHECK | %s | ret=%ld errno=%d (%s), expected=%d\n",  \
               msg, __ret, __err, strerror(__err), (int)(exp_errno));  \
        ASSERT_ACTIVE(__ret == -1);                                     \
        ASSERT_ACTIVE(__err == (exp_errno));                            \
        __pass++;                                                       \
    } while (0)

#define ASSERT_TRUE(cond, msg)                                          \
    do {                                                                \
        int __ok = !!(cond);                                            \
        printf("  CHECK | %s | %s\n", msg, __ok ? "true" : "false"); \
        ASSERT_ACTIVE(__ok);                                            \
        __pass++;                                                       \
    } while (0)

#define TEST_DONE()                                                     \
    do {                                                                \
        printf("------------------------------------------------\n");   \
        printf("  DONE: %d pass, 0 fail\n", __pass);                  \
        printf("================================================\n\n"); \
        return 0;                                                       \
    } while (0)
