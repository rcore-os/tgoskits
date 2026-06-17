#pragma once

#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>

#define T_MODULE(name) printf("\n--- module: %s ---\n", name)

#define CHECK(cond, msg) do { \
    if (cond) { \
        __pass++; \
    } else { \
        printf("  FAIL | %s:%d | %s | errno=%d (%s)\n", \
               __FILE__, __LINE__, msg, errno, strerror(errno)); \
        __fail++; \
    } \
} while(0)

#define CHECK_QUIET(cond, msg) do { \
    if (cond) { \
        __pass++; \
    } else { \
        printf("  FAIL | %s:%d | %s | errno=%d (%s)\n", \
               __FILE__, __LINE__, msg, errno, strerror(errno)); \
        __fail++; \
    } \
} while(0)

#define CHECK_RET(call, expected, msg) do { \
    errno = 0; \
    long _r = (long)(call); \
    long _e = (long)(expected); \
    if (_r == _e) { \
        __pass++; \
    } else { \
        printf("  FAIL | %s:%d | %s | expected=%ld got=%ld | errno=%d (%s)\n", \
               __FILE__, __LINE__, msg, _e, _r, errno, strerror(errno)); \
        __fail++; \
    } \
} while(0)

#define CHECK_ERRNO(call, expected_errno, msg) do { \
    errno = 0; \
    long _r = (long)(call); \
    if (_r == -1 && errno == (expected_errno)) { \
        __pass++; \
    } else { \
        printf("  FAIL | %s:%d | %s | expected errno=%d got ret=%ld errno=%d (%s)\n", \
               __FILE__, __LINE__, msg, (int)(expected_errno), _r, errno, strerror(errno)); \
        __fail++; \
    } \
} while(0)

#define MODULE_START(name) \
    static int __pass = 0; \
    static int __fail = 0; \
    T_MODULE(name)

#define MODULE_RETURN() return __fail

#define MODULE_SUMMARY(name) do { \
    printf("  %s: %d pass, %d fail\n", name, __pass, __fail); \
} while(0)
