/*
 * bug-write-bad-buffer-efault: sys_write dropped its redundant early
 * validate_user_read_buf. The single remaining copy_user_read_buf still
 * validates the user buffer, and — crucially — it runs *before* the memfd
 * seal check, so write(2)'s Linux errno priority is preserved:
 *
 *   1. data-consuming fd + unmapped buffer      -> EFAULT (buffer still checked)
 *   2. valid buffer on an F_SEAL_WRITE memfd     -> EPERM  (seal enforced)
 *   3. unmapped buffer on an F_SEAL_WRITE memfd  -> EFAULT, NOT EPERM
 *
 * Case 3 is the regression this test exists for: the bad-buffer fault must
 * take priority over the seal error, matching Linux generic_perform_write,
 * where fault_in_iov_iter_readable (EFAULT) runs before shmem_write_begin's
 * seal check (EPERM). Moving the copy back *after* memfd_checks_before_
 * stream_write would flip case 3 to EPERM and fail this test.
 *
 * The bad-buffer cases must target an fd that actually consumes the user
 * data. /dev/null never reads the buffer, so on Linux write(fd, badbuf, n)
 * to /dev/null succeeds (returns n) — a memfd is used instead so the copy is
 * really exercised.
 */
#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef F_ADD_SEALS
#define F_ADD_SEALS 1033
#endif
#ifndef F_SEAL_WRITE
#define F_SEAL_WRITE 0x0008
#endif
#ifndef MFD_ALLOW_SEALING
#define MFD_ALLOW_SEALING 0x0002
#endif

enum { TEST_PAGE_SIZE = 4096 };

static int memfd_create_sys(const char *name, unsigned int flags)
{
    return (int)syscall(SYS_memfd_create, name, flags);
}

/* Map then unmap a page to obtain a guaranteed-unmapped user address. */
static void *make_unmapped_page(void)
{
    void *page = mmap(NULL, TEST_PAGE_SIZE, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (page == MAP_FAILED) {
        return MAP_FAILED;
    }
    if (munmap(page, TEST_PAGE_SIZE) != 0) {
        return MAP_FAILED;
    }
    return page;
}

int main(void)
{
    printf("=== bug-write-bad-buffer-efault ===\n");
    printf("Expected: write() still faults a bad buffer with EFAULT, and the\n");
    printf("          bad-buffer fault outranks a memfd write seal (EFAULT,\n");
    printf("          not EPERM), matching Linux write(2).\n\n");

    /* A memfd actually consumes written bytes (unlike /dev/null), so it can
     * observe both a valid write and a faulting buffer. MFD_ALLOW_SEALING lets
     * us add F_SEAL_WRITE below. */
    int fd = memfd_create_sys("bad-buf-efault", MFD_ALLOW_SEALING);
    if (fd < 0) {
        printf("FAIL: memfd_create: %s\n", strerror(errno));
        printf("TEST FAILED\n");
        return 1;
    }
    if (ftruncate(fd, TEST_PAGE_SIZE) != 0) {
        printf("FAIL: ftruncate: %s\n", strerror(errno));
        printf("TEST FAILED\n");
        close(fd);
        return 1;
    }

    /* A known-valid write must still succeed (the dropped check was redundant). */
    const char ok[] = "ok";
    if (write(fd, ok, sizeof(ok)) != (ssize_t)sizeof(ok)) {
        printf("FAIL: valid write to memfd failed: %s\n", strerror(errno));
        printf("TEST FAILED\n");
        close(fd);
        return 1;
    }
    printf("PASS: valid write succeeded\n");

    /* Build a guaranteed-unmapped address by mapping then unmapping a page. */
    void *bad = make_unmapped_page();
    if (bad == MAP_FAILED) {
        printf("FAIL: could not build unmapped page: %s\n", strerror(errno));
        printf("TEST FAILED\n");
        close(fd);
        return 1;
    }

    /* 1) write() from the unmapped buffer to a data-consuming fd must fail with
     *    EFAULT — proves copy_user_read_buf still validates after the de-dup. */
    errno = 0;
    ssize_t rc = write(fd, bad, TEST_PAGE_SIZE);
    if (rc != -1 || errno != EFAULT) {
        printf("FAIL: write(memfd, bad buf) returned %zd errno %d (%s); "
               "want -1/EFAULT\n", rc, errno, strerror(errno));
        printf("      the buffer is no longer validated after the de-dup\n");
        printf("TEST FAILED\n");
        close(fd);
        return 1;
    }
    printf("PASS: unmapped buffer rejected with EFAULT on a data-consuming fd\n");

    /* Seal the memfd against writes (no writable shared mapping is live, so
     * F_ADD_SEALS(F_SEAL_WRITE) succeeds). */
    if (fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE) != 0) {
        printf("FAIL: F_ADD_SEALS(F_SEAL_WRITE): %s\n", strerror(errno));
        printf("TEST FAILED\n");
        close(fd);
        return 1;
    }

    /* 2) A valid buffer on the sealed memfd surfaces the seal error (EPERM).
     *    This is the "no bad buffer" baseline: with nothing to fault, the seal
     *    is what write(2) reports. */
    errno = 0;
    rc = write(fd, ok, sizeof(ok));
    if (rc != -1 || errno != EPERM) {
        printf("FAIL: write(sealed memfd, valid buf) returned %zd errno %d "
               "(%s); want -1/EPERM\n", rc, errno, strerror(errno));
        printf("TEST FAILED\n");
        close(fd);
        return 1;
    }
    printf("PASS: sealed write with a valid buffer rejected with EPERM\n");

    /* 3) The ordering guarantee: an unmapped buffer on the *sealed* memfd must
     *    still report EFAULT, not the seal's EPERM. The bad-buffer fault takes
     *    priority over the seal, matching Linux. Regressing the copy back
     *    *after* the seal check would flip this to EPERM. */
    errno = 0;
    rc = write(fd, bad, TEST_PAGE_SIZE);
    if (rc != -1 || errno != EFAULT) {
        printf("FAIL: write(sealed memfd, bad buf) returned %zd errno %d (%s); "
               "want -1/EFAULT (EFAULT must outrank the seal's EPERM)\n",
               rc, errno, strerror(errno));
        printf("TEST FAILED\n");
        close(fd);
        return 1;
    }
    printf("PASS: sealed memfd + bad buffer keeps EFAULT priority over EPERM\n");

    close(fd);
    printf("TEST PASSED\n");
    return 0;
}
