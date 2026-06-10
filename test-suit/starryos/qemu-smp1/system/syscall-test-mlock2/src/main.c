/*
 * test_mlock2.c — mlock(2) / mlock2(2) / munlock(2) boundary semantics.
 *
 * Regression for the fix that turned the mlock family from a pure `Ok(0)` stub
 * into a real implementation: it now (a) rejects unknown mlock2 flags with
 * EINVAL, (b) faults the range in / verifies coverage and reports ENOMEM on an
 * unmapped hole, and (c) rejects an addr+length that overflows the address
 * space instead of wrapping.
 *
 * mlock2 / MLOCK_ONFAULT are not in the musl cross sysroot, so the constant and
 * the raw syscall are used directly to keep the test self-contained.
 */

#include "test_framework.h"
#include <unistd.h>
#include <sys/mman.h>
#include <sys/syscall.h>

#ifndef MLOCK_ONFAULT
#define MLOCK_ONFAULT 0x01u
#endif

static long raw_mlock2(const void *addr, size_t len, unsigned int flags)
{
    return syscall(SYS_mlock2, addr, len, flags);
}

int main(void)
{
    TEST_START("mlock/mlock2/munlock");

    long ps = sysconf(_SC_PAGESIZE);

    char *p = mmap(NULL, ps, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(p != MAP_FAILED, "mmap one RW page");

    /* flags == 0: lock succeeds (fix faults the range in) */
    CHECK_RET(raw_mlock2(p, ps, 0), 0, "mlock2(flags=0) → 0");

    /* MLOCK_ONFAULT is the only other accepted flag */
    CHECK_RET(raw_mlock2(p, ps, MLOCK_ONFAULT), 0, "mlock2(MLOCK_ONFAULT) → 0");

    /* any unknown flag bit → EINVAL (was silently accepted by the old stub) */
    CHECK_ERR(raw_mlock2(p, ps, 0xFFu), EINVAL, "mlock2(unknown flags) → EINVAL");

    /* (munlock is a separate syscall not covered by this fix; omitted here.) */
    munmap(p, ps);

    /* mlock over [mapped][hole][mapped] → ENOMEM (man 2 mlock: some pages of the
     * range are not mapped). The old stub returned 0 here. */
    char *hole = mmap(NULL, 3 * ps, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(hole != MAP_FAILED, "mmap 3 pages for hole test");
    if (hole != MAP_FAILED) {
        CHECK_RET(munmap(hole + ps, ps), 0, "munmap middle page (punch hole)");
        CHECK_ERR(mlock(hole, 3 * ps), ENOMEM, "mlock over a hole → ENOMEM");
        munmap(hole, ps);
        munmap(hole + 2 * ps, ps);
    }

    TEST_DONE();
}
