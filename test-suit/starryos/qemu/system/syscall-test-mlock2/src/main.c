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
#include <sys/wait.h>

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

    CHECK_ERR(msync(p, ps, MS_INVALIDATE), EBUSY,
              "MS_INVALIDATE rejects an MLOCK_ONFAULT VMA");
    CHECK_RET(munlock(p, ps), 0, "munlock clears MLOCK_ONFAULT state");
    CHECK_RET(msync(p, ps, MS_INVALIDATE), 0,
              "MS_INVALIDATE succeeds after munlock");
    munmap(p, ps);

    /* VM_LOCKED is a VMA property. A partial lock must split the immutable
     * metadata without leaking the state into adjacent fragments. */
    char *partial = mmap(NULL, 3 * ps, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(partial != MAP_FAILED, "mmap 3 pages for partial lock");
    if (partial != MAP_FAILED) {
        CHECK_RET(mlock(partial + ps, ps), 0, "mlock middle page");
        CHECK_RET(msync(partial, ps, MS_INVALIDATE), 0,
                  "left unlocked fragment accepts MS_INVALIDATE");
        CHECK_ERR(msync(partial + ps, 1, MS_INVALIDATE), EBUSY,
                  "partial MS_INVALIDATE of locked VMA returns EBUSY");
        CHECK_RET(msync(partial + 2 * ps, ps, MS_INVALIDATE), 0,
                  "right unlocked fragment accepts MS_INVALIDATE");
        CHECK_RET(munlock(partial + ps, ps), 0, "munlock middle fragment");
        CHECK_RET(msync(partial + ps, ps, MS_INVALIDATE), 0,
                  "unlocked middle fragment accepts MS_INVALIDATE");
        munmap(partial, 3 * ps);
    }

    /* MAP_LOCKED publishes the same persistent state as mlock and faults the
     * mapping in. */
    char *map_locked = mmap(NULL, ps, PROT_READ | PROT_WRITE,
                            MAP_PRIVATE | MAP_ANONYMOUS | MAP_LOCKED, -1, 0);
    CHECK(map_locked != MAP_FAILED, "mmap MAP_LOCKED page");
    if (map_locked != MAP_FAILED) {
        CHECK_ERR(msync(map_locked, ps, MS_INVALIDATE), EBUSY,
                  "MAP_LOCKED VMA rejects MS_INVALIDATE");
        CHECK_RET(munlock(map_locked, ps), 0, "munlock MAP_LOCKED VMA");
        munmap(map_locked, ps);
    }

    /* Linux clears VM_LOCKED_MASK while duplicating VMAs for fork. The child
     * must not inherit the parent's lock, while the parent remains locked. */
    char *fork_map = mmap(NULL, ps, PROT_READ | PROT_WRITE,
                          MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(fork_map != MAP_FAILED, "mmap fork lock fixture");
    if (fork_map != MAP_FAILED) {
        CHECK_RET(mlock(fork_map, ps), 0, "mlock parent fork fixture");
        pid_t child = fork();
        CHECK(child >= 0, "fork locked VMA");
        if (child == 0) {
            _exit(msync(fork_map, ps, MS_INVALIDATE) == 0 ? 0 : 1);
        }
        if (child > 0) {
            int status = 0;
            CHECK_RET(waitpid(child, &status, 0), child, "wait locked-VMA child");
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "fork child does not inherit VM_LOCKED");
            CHECK_ERR(msync(fork_map, ps, MS_INVALIDATE), EBUSY,
                      "fork parent retains VM_LOCKED");
        }
        CHECK_RET(munlock(fork_map, ps), 0, "munlock parent fork fixture");
        munmap(fork_map, ps);
    }

    /* A moved mapping keeps its VMA lock policy. */
    char *move_src = mmap(NULL, ps, PROT_READ | PROT_WRITE,
                          MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    char *move_dst = mmap(NULL, ps, PROT_NONE,
                          MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(move_src != MAP_FAILED && move_dst != MAP_FAILED,
          "mmap mremap lock fixtures");
    if (move_src != MAP_FAILED && move_dst != MAP_FAILED) {
        CHECK_RET(mlock(move_src, ps), 0, "mlock mremap source");
        void *moved = mremap(move_src, ps, ps,
                             MREMAP_MAYMOVE | MREMAP_FIXED, move_dst);
        CHECK(moved == move_dst, "mremap locked VMA to fixed destination");
        if (moved == move_dst) {
            CHECK_ERR(msync(moved, ps, MS_INVALIDATE), EBUSY,
                      "mremap preserves VM_LOCKED");
            CHECK_RET(munlock(moved, ps), 0, "munlock moved VMA");
            munmap(moved, ps);
        } else {
            munmap(move_src, ps);
            munmap(move_dst, ps);
        }
    } else {
        if (move_src != MAP_FAILED) munmap(move_src, ps);
        if (move_dst != MAP_FAILED) munmap(move_dst, ps);
    }

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
