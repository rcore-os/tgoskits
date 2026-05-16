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
    memset(&u, 0, sizeof(u));

    CHECK_RET(syscall(SYS_uname, &u), 0, "uname returns 0");
    CHECK(strcmp(u.sysname, "Linux") == 0, "sysname is Linux");
    CHECK(strcmp(u.nodename, "starry") == 0, "nodename is starry");
    CHECK(strcmp(u.release, "10.0.0") == 0, "release matches");
    CHECK(strcmp(u.version, "10.0.0") == 0, "version matches");
    CHECK(check_nonempty(u.machine), "machine is non-empty");
    CHECK(strcmp(u.domainname, "https://github.com/Starry-OS/StarryOS") == 0,
          "domainname matches");

    CHECK_ERR(syscall(SYS_uname, NULL), EFAULT, "NULL uname pointer returns EFAULT");
    CHECK_ERR(syscall(SYS_uname, (void *)1), EFAULT, "bad uname pointer returns EFAULT");

    TEST_DONE();
}
