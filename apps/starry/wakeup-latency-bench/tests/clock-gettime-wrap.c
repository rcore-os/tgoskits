#define _GNU_SOURCE

#include <errno.h>
#include <time.h>

int __wrap_clock_gettime(clockid_t clock_id, struct timespec *timestamp)
{
    (void)clock_id;
    (void)timestamp;
    errno = ENOSYS;
    return -1;
}
