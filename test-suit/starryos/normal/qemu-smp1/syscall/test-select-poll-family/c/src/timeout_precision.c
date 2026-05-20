#include "test_framework.h"
#include "helpers.h"
#include <sys/select.h>
#include <sys/time.h>
#include <poll.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_timeout_precision(void) {
    MODULE_START("timeout_precision");

    {
        long long t0 = time_ms();
        struct timeval tv = {0, 100000};
        raw_select(0, NULL, NULL, NULL, &tv);
        long long elapsed = time_ms() - t0;
        CHECK(elapsed >= 50 && elapsed <= 500, "select 100ms timeout in reasonable range");
    }

    {
        long long t0 = time_ms();
        raw_poll(NULL, 0, 100);
        long long elapsed = time_ms() - t0;
        CHECK(elapsed >= 50 && elapsed <= 500, "poll 100ms timeout in reasonable range");
    }

    {
        long long t0 = time_ms();
        struct timeval tv = {0, 50000};
        raw_select(0, NULL, NULL, NULL, &tv);
        long long elapsed = time_ms() - t0;
        CHECK(elapsed >= 20 && elapsed <= 500, "select 50ms timeout in reasonable range");
    }

    {
        long long t0 = time_ms();
        raw_poll(NULL, 0, 50);
        long long elapsed = time_ms() - t0;
        CHECK(elapsed >= 20 && elapsed <= 500, "poll 50ms timeout in reasonable range");
    }

    MODULE_SUMMARY("timeout_precision");
    MODULE_RETURN();
}
