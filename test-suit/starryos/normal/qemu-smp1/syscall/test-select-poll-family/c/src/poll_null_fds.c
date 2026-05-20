#include "test_framework.h"
#include "helpers.h"
#include <poll.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_poll_null_fds(void) {
    MODULE_START("poll_null_fds");

    long long t0 = time_ms();
    CHECK_RET(raw_poll(NULL, 0, 50), 0, "poll(NULL,0,50) returns 0");
    long long elapsed = time_ms() - t0;
    CHECK(elapsed >= 40 && elapsed < 200, "poll(NULL,0,50) elapsed ~50ms");

    t0 = time_ms();
    CHECK_RET(raw_poll(NULL, 0, 100), 0, "poll(NULL,0,100) returns 0");
    elapsed = time_ms() - t0;
    CHECK(elapsed >= 80 && elapsed < 300, "poll(NULL,0,100) elapsed ~100ms");

    MODULE_SUMMARY("poll_null_fds");
    MODULE_RETURN();
}
