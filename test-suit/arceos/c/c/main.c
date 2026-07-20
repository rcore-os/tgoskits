#include "test.h"

#include <stdarg.h>
#include <stdio.h>
#include <time.h>

void test_fail(char *reason, size_t reason_len, const char *fmt, ...)
{
    va_list args;

    if (reason_len == 0) {
        return;
    }

    va_start(args, fmt);
    vsnprintf(reason, reason_len, fmt, args);
    va_end(args);
    reason[reason_len - 1] = '\0';
}

static unsigned long elapsed_ms(const struct timespec *started)
{
    struct timespec now;
    long sec;
    long nsec;

    clock_gettime(0, &now);
    sec = now.tv_sec - started->tv_sec;
    nsec = now.tv_nsec - started->tv_nsec;
    if (nsec < 0) {
        sec -= 1;
        nsec += 1000000000L;
    }
    return (unsigned long)sec * 1000UL + (unsigned long)(nsec / 1000000L);
}

static const struct arceos_c_test_case TESTS[] = {
#if defined(ARCEOS_C_TEST_CASE_ALL) || defined(ARCEOS_C_TEST_CASE_MEM)
    {"mem", "memory allocation APIs", arceos_c_test_mem},
#endif
#if defined(ARCEOS_C_TEST_CASE_ALL) || defined(ARCEOS_C_TEST_CASE_PTHREAD_BASIC)
    {"pthread-basic", "pthread create join exit mutex APIs", arceos_c_test_pthread_basic},
#endif
#if defined(ARCEOS_C_TEST_CASE_ALL) || defined(ARCEOS_C_TEST_CASE_PTHREAD_PARALLEL)
    {"pthread-parallel", "pthread parallel compute APIs", arceos_c_test_pthread_parallel},
#endif
#if defined(ARCEOS_C_TEST_CASE_ALL) || defined(ARCEOS_C_TEST_CASE_PTHREAD_SLEEP)
    {"pthread-sleep", "pthread sleep and clock APIs", arceos_c_test_pthread_sleep},
#endif
#if defined(ARCEOS_C_TEST_CASE_ALL) || defined(ARCEOS_C_TEST_CASE_PIPE)
    {"pipe", "pipe read write close APIs", arceos_c_test_pipe},
#endif
#if defined(ARCEOS_C_TEST_CASE_ALL) || defined(ARCEOS_C_TEST_CASE_EPOLL)
    {"epoll", "epoll pipe readiness APIs", arceos_c_test_epoll},
#endif
#if defined(ARCEOS_C_TEST_CASE_ALL) || defined(ARCEOS_C_TEST_CASE_NET_HTTP)
    {"net-http", "socket host HTTP APIs", arceos_c_test_net_http},
#endif
};

int main(void)
{
    size_t count = sizeof(TESTS) / sizeof(TESTS[0]);

    if (count == 0) {
        puts("ARCEOS_C_TEST_FAIL reason=no C test case selected");
        return 1;
    }

    printf("ArceOS C test suite run begin: %lu tests\n", (unsigned long)count);
    for (size_t i = 0; i < count; i++) {
        char reason[192] = {0};
        struct timespec started;
        int ret;
        unsigned long ms;

        clock_gettime(0, &started);
        printf("ARCEOS_C_TEST_BEGIN feature=%s name=%s\n", TESTS[i].feature, TESTS[i].name);
        ret = TESTS[i].run(reason, sizeof(reason));
        ms = elapsed_ms(&started);
        if (ret == 0) {
            printf("ARCEOS_C_TEST_END feature=%s name=%s status=pass elapsed_ms=%lu\n",
                   TESTS[i].feature, TESTS[i].name, ms);
            continue;
        }

        if (reason[0] == '\0') {
            test_fail(reason, sizeof(reason), "test returned %d", ret);
        }
        printf("ARCEOS_C_TEST_END feature=%s name=%s status=fail elapsed_ms=%lu reason=%s\n",
               TESTS[i].feature, TESTS[i].name, ms, reason);
        printf("ARCEOS_C_TEST_FAIL feature=%s reason=%s\n", TESTS[i].feature, reason);
        return 1;
    }

    puts("ArceOS C test suite run OK!");
    return 0;
}
