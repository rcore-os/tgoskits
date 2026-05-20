#include "test_framework.h"
#include "helpers.h"
#include <sys/select.h>
#include <sys/time.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_select_err_einval(void) {
    MODULE_START("select_err_einval");

    fd_set rfds;
    FD_ZERO(&rfds);

    CHECK_ERRNO(raw_select(-1, &rfds, NULL, NULL, NULL), EINVAL, "nfds=-1 returns EINVAL");

    struct timeval tv;
    tv.tv_sec = 0;
    tv.tv_usec = -1;
    CHECK_ERRNO(raw_select(1, &rfds, NULL, NULL, &tv), EINVAL, "tv_usec=-1 returns EINVAL");

    tv.tv_sec = 0;
    tv.tv_usec = 1000000;
    errno = 0;
    long ret = raw_select(1, &rfds, NULL, NULL, &tv);
    CHECK((ret == -1 && errno == EINVAL) || ret >= 0,
          "tv_usec=1000000: EINVAL or normalized by kernel");

    MODULE_SUMMARY("select_err_einval");
    MODULE_RETURN();
}
