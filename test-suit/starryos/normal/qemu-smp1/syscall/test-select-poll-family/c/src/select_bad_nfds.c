#include "test_framework.h"
#include "helpers.h"
#include <sys/select.h>
#include <sys/time.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_select_bad_nfds(void) {
    MODULE_START("select_bad_nfds");

    fd_set rfds;
    FD_ZERO(&rfds);

    CHECK_ERRNO(raw_select(-1, &rfds, NULL, NULL, NULL), EINVAL, "select nfds=-1 returns EINVAL");

    struct timeval tv = {0, 1000000};
    errno = 0;
    long ret = raw_select(1, &rfds, NULL, NULL, &tv);
    CHECK((ret == -1 && errno == EINVAL) || ret >= 0,
          "select tv_usec>=1000000: EINVAL or normalized");

    tv.tv_sec = -1;
    tv.tv_usec = 0;
    CHECK_ERRNO(raw_select(1, &rfds, NULL, NULL, &tv), EINVAL, "select tv_sec=-1 returns EINVAL");

    MODULE_SUMMARY("select_bad_nfds");
    MODULE_RETURN();
}
