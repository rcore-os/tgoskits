#define _GNU_SOURCE
#include "test_framework.h"
#include <fcntl.h>
#include <signal.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/timerfd.h>
#include <time.h>
#include <unistd.h>

/*
 * Syscall numbers (Linux asm-generic / x86_64 tables).
 * The header values are duplicated here because the guest musl headers do
 * not expose the newest names (listxattrat/removexattrat).
 */
#if defined(__x86_64__)
#define SYS_READAHEAD_NR       187
#define SYS_REMAP_FILE_PAGES_NR 216
#define SYS_TIMER_GETOVERRUN_NR 225
#define SYS_SIGNALFD_NR        282
#define SYS_IOPRIO_SET_NR      251
#define SYS_IOPRIO_GET_NR      252
#define SYS_LISTXATTRAT_NR     465
#define SYS_REMOVEXATTRAT_NR   466
#elif defined(__riscv) || defined(__aarch64__) || defined(__loongarch64)
#define SYS_READAHEAD_NR       213
#define SYS_REMAP_FILE_PAGES_NR 234
#define SYS_TIMER_GETOVERRUN_NR 109
#define SYS_IOPRIO_SET_NR      30
#define SYS_IOPRIO_GET_NR      31
#define SYS_LISTXATTRAT_NR     465
#define SYS_REMOVEXATTRAT_NR   466
#else
#error "unsupported architecture for small syscall family test"
#endif

int main(void)
{
    TEST_START("readahead / remap_file_pages / timer_getoverrun / ioprio / xattr-at");

    /* readahead: valid fd succeeds, count==0 is a no-op, bad fd is EBADF. */
    {
        int fd = open("/tmp/syscall-small-family.bin", O_CREAT | O_RDWR, 0600);
        CHECK(fd >= 0, "open scratch file");
        if (fd >= 0) {
            CHECK_RET(syscall(SYS_READAHEAD_NR, fd, 0, 0), 0,
                      "readahead(fd, 0, 0)");
            CHECK_RET(syscall(SYS_READAHEAD_NR, fd, 0, 4096), 0,
                      "readahead(fd, 0, 4096)");
            CHECK_ERR(syscall(SYS_READAHEAD_NR, -1, 0, 4096), EBADF,
                      "readahead(-1) -> EBADF");
            close(fd);
            unlink("/tmp/syscall-small-family.bin");
        }
    }

    /* remap_file_pages: legacy no-op, but ABI validation still applies. */
    {
        void *p = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(p != MAP_FAILED, "anonymous mmap");
        if (p != MAP_FAILED) {
            CHECK_RET(syscall(SYS_REMAP_FILE_PAGES_NR, (unsigned long)p,
                              4096, 0, 0, 0), 0,
                      "remap_file_pages(valid)");
            CHECK_ERR(syscall(SYS_REMAP_FILE_PAGES_NR, (unsigned long)p,
                              4096, 0, 0, 0xffff), EINVAL,
                      "remap_file_pages(bad flags) -> EINVAL");
            munmap(p, 4096);
        }
    }

    /* timer_getoverrun: valid timer id returns >= 0, unknown id EINVAL. */
    {
        timer_t tid;
        CHECK_RET(timer_create(CLOCK_MONOTONIC, NULL, &tid), 0,
                  "timer_create");
        if (tid != (timer_t)-1) {
            CHECK_RET(syscall(SYS_TIMER_GETOVERRUN_NR, (long)tid), 0,
                      "timer_getoverrun(valid)");
            CHECK_ERR(syscall(SYS_TIMER_GETOVERRUN_NR, 0x7fffffff), EINVAL,
                      "timer_getoverrun(unknown) -> EINVAL");
            timer_delete(tid);
        }
    }

    /* signalfd legacy entry point (x86_64 only). */
#if defined(__x86_64__)
    {
        sigset_t mask;
        sigemptyset(&mask);
        sigaddset(&mask, SIGUSR1);
        int fd = (int)syscall(SYS_SIGNALFD_NR, -1, &mask, sizeof(mask));
        CHECK(fd >= 0, "signalfd(-1) creates descriptor");
        if (fd >= 0) {
            close(fd);
        }
    }
#endif

    /* ioprio_get/set: default priority and ABI validation. */
    {
        CHECK_RET(syscall(SYS_IOPRIO_GET_NR, 1, 0), 0,
                  "ioprio_get(process, self)");
        CHECK_RET(syscall(SYS_IOPRIO_SET_NR, 1, 0, 0), 0,
                  "ioprio_set(process, self, none)");
        CHECK_RET(syscall(SYS_IOPRIO_SET_NR, 1, 0, (2u << 13) | 7), 0,
                  "ioprio_set(process, self, BE/7)");
        CHECK_ERR(syscall(SYS_IOPRIO_SET_NR, 1, 0, (4u << 13)), EINVAL,
                  "ioprio_set(bad class) -> EINVAL");
        CHECK_ERR(syscall(SYS_IOPRIO_GET_NR, 0, 0), EINVAL,
                  "ioprio_get(bad which) -> EINVAL");
    }

    /* listxattrat/removexattrat: dispatch through path resolution. */
    {
        CHECK_ERR(syscall(SYS_LISTXATTRAT_NR, AT_FDCWD,
                          "/no/such/path", NULL, 0, 0), ENOENT,
                  "listxattrat(missing path) -> ENOENT");
        CHECK_ERR(syscall(SYS_REMOVEXATTRAT_NR, AT_FDCWD,
                          "/no/such/path", "user.x", 0), ENOENT,
                  "removexattrat(missing path) -> ENOENT");
    }

    TEST_DONE();
}
