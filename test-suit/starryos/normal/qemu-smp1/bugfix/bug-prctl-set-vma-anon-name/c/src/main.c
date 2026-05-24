#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef PR_SET_VMA
#define PR_SET_VMA 0x53564d41
#endif
#ifndef PR_SET_VMA_ANON_NAME
#define PR_SET_VMA_ANON_NAME 0
#endif

static int passed;
static int failed;

static void note_pass(const char *name)
{
    printf("PASS: %s\n", name);
    passed++;
}

static void note_fail(const char *name, const char *detail)
{
    printf("FAIL: %s: %s\n", name, detail);
    failed++;
}

static long prctl_raw(int option, unsigned long arg2, unsigned long arg3,
                      unsigned long arg4, unsigned long arg5)
{
    return syscall(SYS_prctl, option, arg2, arg3, arg4, arg5);
}

static void test_vma_anon_name_ok(void)
{
    size_t len = 4096;
    void *addr = mmap(NULL, len, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (addr == MAP_FAILED) {
        note_fail("PR_SET_VMA ANON_NAME", "mmap failed");
        return;
    }

    errno = 0;
    long ret = prctl_raw(PR_SET_VMA, PR_SET_VMA_ANON_NAME,
                         (unsigned long)addr, len, (unsigned long)"picoclaw-test");
    if (ret == 0) {
        note_pass("PR_SET_VMA ANON_NAME returns 0");
    } else {
        char detail[160];
        snprintf(detail, sizeof(detail),
                 "ret=%ld errno=%d (%s), expected 0",
                 ret, errno, strerror(errno));
        note_fail("PR_SET_VMA ANON_NAME", detail);
    }

    munmap(addr, len);
}

static void test_vma_unknown_subop_einval(void)
{
    size_t len = 4096;
    void *addr = mmap(NULL, len, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (addr == MAP_FAILED) {
        note_fail("PR_SET_VMA unknown subop", "mmap failed");
        return;
    }

    errno = 0;
    long ret = prctl_raw(PR_SET_VMA, 9999,
                         (unsigned long)addr, len, (unsigned long)"bad");
    int saved = errno;
    if (ret == -1 && saved == EINVAL) {
        note_pass("PR_SET_VMA unknown subop returns EINVAL");
    } else {
        char detail[160];
        snprintf(detail, sizeof(detail),
                 "ret=%ld errno=%d (%s), expected -1/EINVAL",
                 ret, saved, strerror(saved));
        note_fail("PR_SET_VMA unknown subop", detail);
    }

    munmap(addr, len);
}

int main(void)
{
    printf("=== bug-prctl-set-vma-anon-name ===\n");

    test_vma_anon_name_ok();
    test_vma_unknown_subop_einval();

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("ALL TESTS PASSED\n");
        return 0;
    }
    printf("SOME TESTS FAILED\n");
    return 1;
}
