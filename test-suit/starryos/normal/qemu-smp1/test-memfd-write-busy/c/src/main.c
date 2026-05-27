/* F_SEAL_WRITE EBUSY-on-extant-mapping regression.
 *
 * Linux rejects `F_ADD_SEALS(F_SEAL_WRITE)` with EBUSY when any live
 * MAP_SHARED|PROT_WRITE mapping for the memfd exists (i_writecount > 0),
 * and accepts the seal once every such mapping has been munmapped. This
 * test exercises both directions:
 *   1. memfd_create + ftruncate.
 *   2. mmap PROT_WRITE|MAP_SHARED.
 *   3. F_ADD_SEALS(F_SEAL_WRITE) → expect -1 / errno=EBUSY.
 *   4. munmap; the writable mapping is gone.
 *   5. F_ADD_SEALS(F_SEAL_WRITE) → expect success and F_SEAL_WRITE set.
 *   6. New mmap(PROT_WRITE|MAP_SHARED) → expect -1 / errno=EPERM.
 */

#include "test_framework.h"

#include <fcntl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef F_ADD_SEALS
#define F_ADD_SEALS 1033
#endif
#ifndef F_GET_SEALS
#define F_GET_SEALS 1034
#endif
#ifndef F_SEAL_WRITE
#define F_SEAL_WRITE 0x0008
#endif
#ifndef MFD_CLOEXEC
#define MFD_CLOEXEC 0x0001
#endif
#ifndef MFD_ALLOW_SEALING
#define MFD_ALLOW_SEALING 0x0002
#endif
#ifndef SYS_memfd_create
#define SYS_memfd_create 279
#endif

static int memfd_create_sys(const char *name, unsigned int flags) {
    return (int)syscall(SYS_memfd_create, name, flags);
}

int main(void) {
    TEST_START("memfd-write-busy");

    int fd = memfd_create_sys("busy", MFD_CLOEXEC | MFD_ALLOW_SEALING);
    CHECK(fd >= 0, "memfd_create");
    if (fd < 0) {
        TEST_DONE();
    }

    CHECK_RET(ftruncate(fd, 4096), 0, "ftruncate 4096");

    void *p = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    CHECK(p != MAP_FAILED, "mmap(PROT_WRITE|MAP_SHARED)");

    /* The mapping is live — F_SEAL_WRITE must EBUSY. */
    errno = 0;
    int rc = fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE);
    CHECK(rc == -1 && errno == EBUSY,
          "F_ADD_SEALS(F_SEAL_WRITE) rejected with EBUSY while mapping is live");

    int seals = fcntl(fd, F_GET_SEALS, 0);
    CHECK((seals & F_SEAL_WRITE) == 0,
          "F_SEAL_WRITE NOT set after EBUSY");

    if (p != MAP_FAILED) CHECK_RET(munmap(p, 4096), 0, "munmap");

    /* After munmap, the writable mapping is gone — the seal must take. */
    errno = 0;
    rc = fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE);
    CHECK_RET(rc, 0, "F_ADD_SEALS(F_SEAL_WRITE) succeeds after munmap");

    seals = fcntl(fd, F_GET_SEALS, 0);
    CHECK((seals & F_SEAL_WRITE) != 0,
          "F_SEAL_WRITE set after seal succeeds");

    /* Sealed: new MAP_SHARED|PROT_WRITE must be refused. */
    errno = 0;
    void *q = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    CHECK(q == MAP_FAILED && errno == EPERM,
          "mmap(PROT_WRITE|MAP_SHARED) refused with EPERM after F_SEAL_WRITE");

    close(fd);

    /* Scenario 2: partial unmap must not let F_SEAL_WRITE through while
     * the other half of the original mapping is still live. This guards
     * against a split-bumps-counter bug (count > 1, never reaches 0
     * after a single munmap) and against a split-loses-guard bug
     * (counter drops to 0 even though a writable mapping remains). */
    fd = memfd_create_sys("split", MFD_CLOEXEC | MFD_ALLOW_SEALING);
    CHECK(fd >= 0, "memfd_create split");
    CHECK_RET(ftruncate(fd, 8192), 0, "ftruncate 8192");

    char *r = mmap(NULL, 8192, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    CHECK(r != MAP_FAILED, "mmap 2 pages PROT_WRITE|MAP_SHARED");

    /* Unmap first page only — should split the VMA but leave the
     * writable mapping count at 1 (one piece still mapped). */
    CHECK_RET(munmap(r, 4096), 0, "munmap first page (split)");

    errno = 0;
    rc = fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE);
    CHECK(rc == -1 && errno == EBUSY,
          "F_SEAL_WRITE still EBUSY after partial unmap (other half live)");

    /* Unmap the remaining page. Now no writable mapping exists. */
    CHECK_RET(munmap(r + 4096, 4096), 0, "munmap second page");

    errno = 0;
    rc = fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE);
    CHECK_RET(rc, 0, "F_SEAL_WRITE succeeds after both halves unmapped");

    close(fd);

    /* Scenario 3: `mprotect` must not be able to upgrade a read-only
     * MAP_SHARED mapping into a writable one once F_SEAL_WRITE has
     * been applied; and even before the seal lands, an `mprotect`
     * upgrade must register with the writable-mapping counter so a
     * later F_ADD_SEALS(F_SEAL_WRITE) sees the live writable mapping
     * and rejects with EBUSY. */
    fd = memfd_create_sys("mprotect", MFD_CLOEXEC | MFD_ALLOW_SEALING);
    CHECK(fd >= 0, "memfd_create mprotect");
    CHECK_RET(ftruncate(fd, 4096), 0, "ftruncate 4096 mprotect");

    /* RO MAP_SHARED — does not count as a writable mapping yet. */
    char *ro = mmap(NULL, 4096, PROT_READ, MAP_SHARED, fd, 0);
    CHECK(ro != MAP_FAILED, "mmap(PROT_READ|MAP_SHARED)");

    /* Counter is 0 — seal succeeds. */
    errno = 0;
    rc = fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE);
    CHECK_RET(rc, 0, "F_SEAL_WRITE succeeds on RO-only mapping");

    /* Post-seal upgrade must be refused. Linux returns EACCES; we
     * surface EPERM through the seal check. Either is acceptable per
     * Linux's documented set; accept both so this passes on either. */
    errno = 0;
    int prot_rc = mprotect(ro, 4096, PROT_READ | PROT_WRITE);
    CHECK(prot_rc == -1 && (errno == EPERM || errno == EACCES),
          "mprotect(PROT_WRITE) refused on sealed memfd mapping");

    CHECK_RET(munmap(ro, 4096), 0, "munmap mprotect ro");
    close(fd);

    /* Pre-seal mprotect upgrade: counter must bump, so the later seal
     * EBUSYs while the upgraded mapping is live, then succeeds after
     * we unmap. */
    fd = memfd_create_sys("upgrade", MFD_CLOEXEC | MFD_ALLOW_SEALING);
    CHECK(fd >= 0, "memfd_create upgrade");
    CHECK_RET(ftruncate(fd, 4096), 0, "ftruncate 4096 upgrade");

    char *up = mmap(NULL, 4096, PROT_READ, MAP_SHARED, fd, 0);
    CHECK(up != MAP_FAILED, "mmap(PROT_READ|MAP_SHARED) for upgrade");

    /* Upgrade RO → RW. No seal in place yet, so this must succeed
     * and bump the writable-mapping counter. */
    CHECK_RET(mprotect(up, 4096, PROT_READ | PROT_WRITE), 0,
              "mprotect(PROT_WRITE) upgrades unsealed mapping");
    up[0] = 'x';

    /* Now F_SEAL_WRITE must EBUSY: the upgraded mapping is live. */
    errno = 0;
    rc = fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE);
    CHECK(rc == -1 && errno == EBUSY,
          "F_SEAL_WRITE EBUSYs while mprotect-upgraded mapping is live");

    CHECK_RET(munmap(up, 4096), 0, "munmap upgrade");

    /* After munmap the writable mapping is gone — seal takes. */
    errno = 0;
    rc = fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE);
    CHECK_RET(rc, 0, "F_SEAL_WRITE succeeds after mprotect-upgrade unmapped");

    close(fd);

    /* Scenario 4: split-then-downgrade-then-partial-unmap.
     *   2 pages RW MAP_SHARED → mprotect right page RO → munmap right.
     * The left page is still writable, so F_ADD_SEALS(F_SEAL_WRITE)
     * must still EBUSY. Earlier code shared one MemfdRegistration Arc
     * across split fragments, which let downgrading + unmapping the
     * right fragment drop the count to 0 while the left was still
     * writable. */
    fd = memfd_create_sys("split-mprotect", MFD_CLOEXEC | MFD_ALLOW_SEALING);
    CHECK(fd >= 0, "memfd_create split-mprotect");
    CHECK_RET(ftruncate(fd, 8192), 0, "ftruncate 8192 split-mprotect");

    char *both = mmap(NULL, 8192, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    CHECK(both != MAP_FAILED, "mmap 2 pages PROT_WRITE|MAP_SHARED");
    both[0] = 'L';
    both[4096] = 'R';

    CHECK_RET(mprotect(both + 4096, 4096, PROT_READ), 0,
              "mprotect right page → PROT_READ (split)");

    /* Unmap the (RO) right page. The left page is still writable, so the
     * count must remain >0 and the seal must EBUSY. */
    CHECK_RET(munmap(both + 4096, 4096), 0, "munmap right RO page");
    errno = 0;
    rc = fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE);
    CHECK(rc == -1 && errno == EBUSY,
          "F_SEAL_WRITE EBUSYs while left writable page still mapped");

    /* Unmap the left page — count drops to 0, seal lands. */
    CHECK_RET(munmap(both, 4096), 0, "munmap left RW page");
    errno = 0;
    rc = fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE);
    CHECK_RET(rc, 0, "F_SEAL_WRITE succeeds after both pages unmapped");

    close(fd);

    TEST_DONE();
}
