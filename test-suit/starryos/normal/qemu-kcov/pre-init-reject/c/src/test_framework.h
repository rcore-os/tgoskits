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
#define CHECK(cond, msg) do { \
    if (cond) { printf("  PASS | %s:%d | %s\n", __FILE__, __LINE__, msg); __pass++; } \
    else { printf("  FAIL | %s:%d | %s | errno=%d (%s)\n", __FILE__, __LINE__, msg, errno, strerror(errno)); __fail++; } \
} while(0)
#define CHECK_RET(call, exp, msg) do { \
    errno = 0; long _r = (long)(call); \
    if (_r == (long)(exp)) { printf("  PASS | %s:%d | %s (ret=%ld)\n", __FILE__, __LINE__, msg, _r); __pass++; } \
    else { printf("  FAIL | %s:%d | %s | exp=%ld got=%ld errno=%d\n", __FILE__, __LINE__, msg, (long)(exp), _r, errno); __fail++; } \
} while(0)
#define CHECK_ERR(call, exp_e, msg) do { \
    errno = 0; long _r = (long)(call); \
    if (_r == -1 && errno == (exp_e)) { printf("  PASS | %s:%d | %s (errno=%d)\n", __FILE__, __LINE__, msg, errno); __pass++; } \
    else { printf("  FAIL | %s:%d | %s | exp errno=%d got ret=%ld errno=%d (%s)\n", __FILE__, __LINE__, msg, (int)(exp_e), _r, errno, strerror(errno)); __fail++; } \
} while(0)
#define CHECK_PTR(ptr, ok, msg) do { \
    if (!!(ptr)==!!(ok)) { printf("  PASS | %s:%d | %s\n", __FILE__, __LINE__, msg); __pass++; } \
    else { printf("  FAIL | %s:%d | %s\n", __FILE__, __LINE__, msg); __fail++; } \
} while(0)
#define TEST_START(n) do { \
    printf("================================================\n  TEST: %s\n================================================\n", n); \
} while(0)
#define TEST_DONE() do { \
    printf("------------------------------------------------\n  DONE: %d pass, %d fail\n================================================\n", __pass, __fail); \
    return __fail > 0 ? 1 : 0; \
} while(0)
