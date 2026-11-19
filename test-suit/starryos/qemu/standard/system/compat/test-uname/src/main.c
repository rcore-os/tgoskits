#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include "test_framework.h"
#include <errno.h>
#include <string.h>
#include <sys/utsname.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SYS_uname
#ifdef __NR_uname
#define SYS_uname __NR_uname
#endif
#endif

static int check_nonempty(const char *s) {
    return s[0] != '\0';
}

int main(void) {
    TEST_START("uname");

    struct utsname u;
    struct utsname u2;
    memset(&u, 0, sizeof(u));
    memset(&u2, 0, sizeof(u2));

    CHECK_RET(syscall(SYS_uname, &u), 0, "uname returns 0");
    CHECK(check_nonempty(u.sysname), "sysname is non-empty");
    CHECK(check_nonempty(u.nodename), "nodename is non-empty");
    CHECK(check_nonempty(u.release), "release is non-empty");
    CHECK(check_nonempty(u.version), "version is non-empty");
    CHECK(check_nonempty(u.machine), "machine is non-empty");
    CHECK(check_nonempty(u.domainname), "domainname is non-empty");

    CHECK_RET(syscall(SYS_uname, &u2), 0, "second uname returns 0");
    CHECK(strcmp(u2.sysname, u.sysname) == 0, "sysname is stable across calls");
    CHECK(strcmp(u2.nodename, u.nodename) == 0, "nodename is stable across calls");
    CHECK(strcmp(u2.release, u.release) == 0, "release is stable across calls");
    CHECK(strcmp(u2.version, u.version) == 0, "version is stable across calls");
    CHECK(strcmp(u2.machine, u.machine) == 0, "machine is stable across calls");
    CHECK(strcmp(u2.domainname, u.domainname) == 0, "domainname is stable across calls");

    CHECK_ERR(syscall(SYS_uname, NULL), EFAULT, "NULL uname pointer returns EFAULT");
    CHECK_ERR(syscall(SYS_uname, (void *)1), EFAULT, "bad uname pointer returns EFAULT");

    TEST_DONE();
}
