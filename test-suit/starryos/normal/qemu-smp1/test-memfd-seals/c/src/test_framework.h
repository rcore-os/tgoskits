#pragma once

#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>

static int __pass = 0;
static int __fail = 0;

#define CHECK(cond, msg) do {                                           \
    if (cond) {                                                         \
        printf("  PASS | %s:%d | %s\n", __FILE__, __LINE__, msg);       \
        __pass++;                                                       \
    } else {                                                            \
        printf("  FAIL | %s:%d | %s | errno=%d (%s)\n",                 \
               __FILE__, __LINE__, msg, errno, strerror(errno));        \
        __fail++;                                                       \
    }                                                                   \
} while(0)

#define CHECK_RET(call, expected, msg) do {                             \
    errno = 0;                                                          \
    long _r = (long)(call);                                             \
    long _e = (long)(expected);                                         \
    if (_r == _e) {                                                     \
        printf("  PASS | %s:%d | %s (ret=%ld)\n",                       \
               __FILE__, __LINE__, msg, _r);                            \
        __pass++;                                                       \
    } else {                                                            \
        printf("  FAIL | %s:%d | %s | expected=%ld got=%ld | errno=%d (%s)\n", \
               __FILE__, __LINE__, msg, _e, _r, errno, strerror(errno));\
        __fail++;                                                       \
    }                                                                   \
} while(0)

#define CHECK_ERR(call, exp_errno, msg) do {                            \
    errno = 0;                                                          \
    long _r = (long)(call);                                             \
    if (_r == -1 && errno == (exp_errno)) {                             \
        printf("  PASS | %s:%d | %s (errno=%d as expected)\n",          \
               __FILE__, __LINE__, msg, errno);                         \
        __pass++;                                                       \
    } else {                                                            \
        printf("  FAIL | %s:%d | %s | expected errno=%d got ret=%ld errno=%d (%s)\n", \
               __FILE__, __LINE__, msg, (int)(exp_errno), _r, errno, strerror(errno));\
        __fail++;                                                       \
    }                                                                   \
} while(0)

#define TEST_START(name)                                                \
    printf("================================================\n");      \
    printf("  TEST: %s\n", name);                                       \
    printf("  FILE: %s\n", __FILE__);                                   \
    printf("================================================\n")

#define TEST_DONE()                                                     \
    printf("------------------------------------------------\n");      \
    printf("  DONE: %d pass, %d fail\n", __pass, __fail);               \
    printf("================================================\n\n");    \
    return __fail > 0 ? 1 : 0
