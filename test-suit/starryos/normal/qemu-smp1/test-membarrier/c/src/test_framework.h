#ifndef TEST_FRAMEWORK_H
#define TEST_FRAMEWORK_H

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int __pass, __fail, __skip, __observe;

#define TEST_START(name)                                                              \
  do {                                                                                \
    __pass = __fail = __skip = __observe = 0;                                         \
    printf("=== Testing %s ===\n", name);                                          \
  } while (0)

#define TEST_DONE()                                                                   \
  do {                                                                                \
    printf("DONE: %d pass, %d fail, %d skip, %d observe\n",                          \
           __pass, __fail, __skip, __observe);                                        \
  } while (0)

#define CHECK(cond, msg)                                                              \
  do {                                                                                \
    if (cond) {                                                                       \
      printf("  PASS: %s\n", msg);                                                    \
      __pass++;                                                                       \
    } else {                                                                          \
      printf("  FAIL: %s\n", msg);                                                    \
      __fail++;                                                                       \
    }                                                                                 \
  } while (0)

#define CHECK_RET(call, expected, msg)                                                \
  do {                                                                                \
    long __ret = (long)(call);                                                        \
    if (__ret == (long)(expected)) {                                                  \
      printf("  PASS: %s (ret=%ld)\n", msg, __ret);                                  \
      __pass++;                                                                       \
    } else {                                                                          \
      printf("  FAIL: %s (expected ret=%ld, got %ld, errno=%d)\n",                   \
             msg, (long)(expected), __ret, errno);                                    \
      __fail++;                                                                       \
    }                                                                                 \
  } while (0)

#define CHECK_ERR(call, exp_errno, msg)                                               \
  do {                                                                                \
    long __ret = (long)(call);                                                        \
    long __e = __ret == -1 ? errno : 0;                                               \
    if (__ret == -1 && __e == (exp_errno)) {                                          \
      printf("  PASS: %s (got expected errno %ld)\n", msg, (long)(exp_errno));       \
      __pass++;                                                                       \
    } else {                                                                          \
      printf("  FAIL: %s (expected %ld, got ret=%ld, errno=%ld)\n",                  \
             msg, (long)(exp_errno), __ret, __e);                                     \
      __fail++;                                                                       \
    }                                                                                 \
  } while (0)

#define TEST_SKIP(msg)                                                                \
  do {                                                                                \
    printf("  SKIP: %s\n", msg);                                                      \
    __skip++;                                                                         \
  } while (0)

#define TEST_OBSERVE(msg)                                                             \
  do {                                                                                \
    printf("  OBSERVE: %s\n", msg);                                                   \
    __observe++;                                                                      \
  } while (0)

#endif
