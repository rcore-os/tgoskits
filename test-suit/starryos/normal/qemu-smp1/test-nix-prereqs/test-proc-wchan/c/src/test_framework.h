#pragma once

#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int __pass = 0;
static int __fail = 0;

#define CHECK(cond, msg)                                                \
    do {                                                                \
        if (cond) {                                                     \
            printf("  PASS | %s:%d | %s\n", __FILE__, __LINE__, msg);  \
            __pass++;                                                   \
        } else {                                                        \
            printf("  FAIL | %s:%d | %s | errno=%d (%s)\n", __FILE__,  \
                   __LINE__, msg, errno, strerror(errno));              \
            __fail++;                                                   \
        }                                                               \
    } while (0)

#define TEST_START(name)                                             \
    do {                                                             \
        printf("================================================\n"); \
        printf("  TEST: %s\n", name);                               \
        printf("  FILE: %s\n", __FILE__);                           \
        printf("================================================\n"); \
    } while (0)

#define TEST_DONE()                                                \
    do {                                                           \
        printf("------------------------------------------------\n"); \
        printf("  DONE: %d pass, %d fail\n", __pass, __fail);     \
        printf("%s\n", __fail > 0 ? "TEST FAILED" : "TEST PASSED"); \
        printf("================================================\n\n"); \
        return __fail > 0 ? 1 : 0;                                \
    } while (0)
