#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include "test_framework.h"
#include <errno.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/sysinfo.h>
#include <unistd.h>

#ifndef SYS_sysinfo
#ifdef __NR_sysinfo
#define SYS_sysinfo __NR_sysinfo
#endif
#endif

static int is_nonnegative(long value) {
    return value >= 0;
}

int main(void) {
    TEST_START("sysinfo");

    struct sysinfo info;
    struct sysinfo info2;
    memset(&info, 0, sizeof(info));
    memset(&info2, 0, sizeof(info2));

    CHECK_RET(syscall(SYS_sysinfo, &info), 0, "sysinfo returns 0");
    CHECK(info.mem_unit > 0, "mem_unit is positive");
    CHECK(is_nonnegative(info.uptime), "uptime is non-negative");
    CHECK(info.totalram > 0, "totalram is non-zero");
    CHECK(info.freeram <= info.totalram, "freeram does not exceed totalram");
    CHECK(info.procs > 0, "process count is non-zero");
    CHECK(info.totalram * info.mem_unit >= info.freeram * info.mem_unit,
          "scaled freeram does not exceed scaled totalram");

    CHECK_RET(syscall(SYS_sysinfo, &info2), 0, "second sysinfo returns 0");
    CHECK(info2.uptime >= info.uptime, "uptime is monotonic across calls");
    CHECK(info2.mem_unit == info.mem_unit, "mem_unit is stable across calls");
    CHECK(info2.totalram == info.totalram, "totalram is stable across calls");

    CHECK_ERR(syscall(SYS_sysinfo, NULL), EFAULT, "NULL sysinfo pointer returns EFAULT");
    CHECK_ERR(syscall(SYS_sysinfo, (void *)1), EFAULT, "bad sysinfo pointer returns EFAULT");

    TEST_DONE();
}
