#include "test_framework.h"
#include "helpers.h"
#include <sys/select.h>
#include <sys/time.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_select_empty_all(void) {
    MODULE_START("select_empty_all");

    struct timeval tv = {0, 50000};
    int ret = raw_select(0, NULL, NULL, NULL, &tv);
    CHECK(ret == 0, "select all NULL 50ms timeout returns 0");

    tv.tv_sec = 0;
    tv.tv_usec = 100000;
    ret = raw_select(0, NULL, NULL, NULL, &tv);
    CHECK(ret == 0, "select all NULL 100ms timeout returns 0");

    tv.tv_sec = 0;
    tv.tv_usec = 0;
    ret = raw_select(0, NULL, NULL, NULL, &tv);
    CHECK(ret == 0, "select all NULL zero timeout returns 0");

    MODULE_SUMMARY("select_empty_all");
    MODULE_RETURN();
}
