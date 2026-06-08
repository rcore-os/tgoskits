#ifndef ARCEOS_C_TEST_H
#define ARCEOS_C_TEST_H

#include <stddef.h>

typedef int (*arceos_c_test_fn)(char *reason, size_t reason_len);

struct arceos_c_test_case {
    const char *feature;
    const char *name;
    arceos_c_test_fn run;
};

int arceos_c_test_mem(char *reason, size_t reason_len);
int arceos_c_test_pthread_basic(char *reason, size_t reason_len);
int arceos_c_test_pthread_parallel(char *reason, size_t reason_len);
int arceos_c_test_pthread_sleep(char *reason, size_t reason_len);
int arceos_c_test_pipe(char *reason, size_t reason_len);
int arceos_c_test_epoll(char *reason, size_t reason_len);
int arceos_c_test_net_http(char *reason, size_t reason_len);

void test_fail(char *reason, size_t reason_len, const char *fmt, ...);

#define CHECK_RET(EXPR, EXPECTED)                                                  \
    do {                                                                           \
        long _actual = (long)(EXPR);                                                \
        long _expected = (long)(EXPECTED);                                          \
        if (_actual != _expected) {                                                 \
            test_fail(reason, reason_len, "%s returned %ld, expected %ld", #EXPR, \
                      _actual, _expected);                                         \
            return -1;                                                             \
        }                                                                          \
    } while (0)

#define CHECK_TRUE(EXPR)                                        \
    do {                                                        \
        if (!(EXPR)) {                                          \
            test_fail(reason, reason_len, "check failed: %s", #EXPR); \
            return -1;                                          \
        }                                                       \
    } while (0)

#endif
