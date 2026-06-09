#pragma once

#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>

extern int __pass;
extern int __fail;
extern int __skip;
extern int __observe;

#define CHECK(cond, msg) do {                                           \
    if (cond) {                                                         \
        printf("  PASS | %s:%d | %s\n", __FILE__, __LINE__, msg);      \
        __pass++;                                                       \
    } else {                                                            \
        printf("  FAIL | %s:%d | %s | errno=%d (%s)\n",                \
               __FILE__, __LINE__, msg, errno, strerror(errno));        \
        __fail++;                                                       \
    }                                                                   \
} while(0)

#define CHECK_RET(call, expected, msg) do {                             \
    errno = 0;                                                          \
    long _r = (long)(call);                                             \
    long _e = (long)(expected);                                         \
    if (_r == _e) {                                                     \
        printf("  PASS | %s:%d | %s (ret=%ld)\n",                      \
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
        printf("  PASS | %s:%d | %s (errno=%d as expected)\n",         \
               __FILE__, __LINE__, msg, errno);                         \
        __pass++;                                                       \
    } else {                                                            \
        printf("  FAIL | %s:%d | %s | expected errno=%d got ret=%ld errno=%d (%s)\n", \
               __FILE__, __LINE__, msg, (int)(exp_errno), _r, errno, strerror(errno));\
        __fail++;                                                       \
    }                                                                   \
} while(0)

/* ====== 新增宏（§1.1.1 断言表到代码映射必需） ====== */

/* 布尔断言：条件为真 */
#define CHECK_TRUE(cond, msg) do {                                      \
    if (cond) {                                                         \
        printf("  PASS | %s:%d | %s\n", __FILE__, __LINE__, msg);      \
        __pass++;                                                       \
    } else {                                                            \
        printf("  FAIL | %s:%d | %s | errno=%d (%s)\n",                \
               __FILE__, __LINE__, msg, errno, strerror(errno));        \
        __fail++;                                                       \
    }                                                                   \
} while(0)

/* 已保存 errno 的错误码断言（用于 syscall 返回后立即保存的场景） */
#define CHECK_ERR_SAVED(ret, err, exp_errno, msg) do {                  \
    if ((ret) == -1 && (err) == (exp_errno)) {                          \
        printf("  PASS | %s:%d | %s (errno=%d as expected)\n",         \
               __FILE__, __LINE__, msg, (int)(err));                    \
        __pass++;                                                       \
    } else {                                                            \
        printf("  FAIL | %s:%d | %s | expected errno=%d got ret=%ld errno=%d (%s)\n", \
               __FILE__, __LINE__, msg, (int)(exp_errno), (long)(ret),  \
               (int)(err), strerror(err));                              \
        __fail++;                                                       \
    }                                                                   \
} while(0)

/* 已保存 errno 的变体错误码断言（errno 为 e1 或 e2 均可） */
#define CHECK_ERR_OR(ret, err, e1, e2, msg) do {                        \
    if ((ret) == -1 && ((err) == (e1) || (err) == (e2))) {              \
        printf("  PASS | %s:%d | %s (errno=%d in {%d,%d})\n",          \
               __FILE__, __LINE__, msg, (int)(err), (int)(e1), (int)(e2)); \
        __pass++;                                                       \
    } else {                                                            \
        printf("  FAIL | %s:%d | %s | expected errno=%d or %d got ret=%ld errno=%d (%s)\n", \
               __FILE__, __LINE__, msg, (int)(e1), (int)(e2),           \
               (long)(ret), (int)(err), strerror(err));                 \
        __fail++;                                                       \
    }                                                                   \
} while(0)

/* 跳过项：输出 SKIP，不计入失败数 */
#define TEST_SKIP(msg) do {                                             \
    printf("  SKIP | %s:%d | %s\n", __FILE__, __LINE__, msg);          \
    __skip++;                                                           \
} while(0)

/* 观察项：输出 OBSERVE，不计入失败数 */
#define TEST_OBSERVE(msg) do {                                          \
    printf("  OBSERVE | %s:%d | %s\n", __FILE__, __LINE__, msg);       \
    __observe++;                                                        \
} while(0)

#define TEST_START(name)                                                \
    printf("================================================\n");       \
    printf("  TEST: %s\n", name);                                       \
    printf("  FILE: %s\n", __FILE__);                                   \
    printf("================================================\n")

#define TEST_DONE()                                                     \
    printf("------------------------------------------------\n");       \
    printf("  DONE: %d pass, %d fail", __pass, __fail);                \
    if (__skip > 0 || __observe > 0) {                                  \
        printf(", %d skip, %d observe", __skip, __observe);            \
    }                                                                   \
    printf("\n");                                                       \
    printf("================================================\n\n");     \
    return __fail > 0 ? 1 : 0
