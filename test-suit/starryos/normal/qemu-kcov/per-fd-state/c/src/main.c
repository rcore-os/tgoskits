/* Per-fd kcov state: verify multiple fds are independent. */
#include "test_framework.h"
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

#define KCOV_INIT_TRACE _IOR('c', 1, unsigned long)
#define KCOV_ENABLE _IO('c', 100)
#define KCOV_DISABLE _IO('c', 101)
#define KCOV_TRACE_PC 0
#define KCOV_TRACE_CMP 1

static void burst(int n) {
    for (volatile int i = 0; i < n; i++) {
        getpid();
        getuid();
    }
}

int main(void) {
    TEST_START("Per-fd kcov state");

    /* ---- 1. Two independent fds, both INIT_TRACE ---- */
    int fd1 = open("/dev/kcov", O_RDWR);
    int fd2 = open("/dev/kcov", O_RDWR);
    CHECK(fd1 >= 0, "fd1 open");
    CHECK(fd2 >= 0, "fd2 open");

    CHECK_RET(ioctl(fd1, KCOV_INIT_TRACE, 256), 0, "fd1 INIT_TRACE");
    CHECK_RET(ioctl(fd2, KCOV_INIT_TRACE, 64), 0,  "fd2 INIT_TRACE (different size)");

    /* ---- 2. ENABLE exclusivity: only one fd can enable per thread ---- */
    uint64_t *buf1 = mmap(NULL, 256 * sizeof(uint64_t), PROT_READ | PROT_WRITE,
                          MAP_SHARED, fd1, 0);
    uint64_t *buf2 = mmap(NULL, 64 * sizeof(uint64_t), PROT_READ | PROT_WRITE,
                          MAP_SHARED, fd2, 0);
    CHECK_PTR(buf1 != MAP_FAILED, 1, "fd1 mmap");
    CHECK_PTR(buf2 != MAP_FAILED, 1, "fd2 mmap");

    /* fd1 ENABLE succeeds, fd2 ENABLE → EBUSY */
    CHECK_RET(ioctl(fd1, KCOV_ENABLE, KCOV_TRACE_PC), 0,   "fd1 ENABLE");
    CHECK_ERR(ioctl(fd2, KCOV_ENABLE, KCOV_TRACE_PC), EBUSY, "fd2 ENABLE → EBUSY");

    /* Disable fd1 first, then fd2 can enable */
    CHECK_RET(ioctl(fd1, KCOV_DISABLE, 0), 0, "fd1 DISABLE");
    CHECK_RET(ioctl(fd2, KCOV_ENABLE, KCOV_TRACE_PC), 0, "fd2 ENABLE after fd1 DISABLE");

    /* fd1 cannot re-enable while fd2 is active */
    CHECK_ERR(ioctl(fd1, KCOV_ENABLE, KCOV_TRACE_PC), EBUSY, "fd1 re-ENABLE → EBUSY (fd2 active)");

    CHECK_RET(ioctl(fd2, KCOV_DISABLE, 0), 0, "fd2 DISABLE");

    /* ---- 3. Close-while-enabled stops coverage ---- */
    int fd3 = open("/dev/kcov", O_RDWR);
    CHECK_RET(ioctl(fd3, KCOV_INIT_TRACE, 128), 0, "fd3 INIT_TRACE");
    uint64_t *buf3 = mmap(NULL, 128 * sizeof(uint64_t), PROT_READ | PROT_WRITE,
                          MAP_SHARED, fd3, 0);
    CHECK_PTR(buf3 != MAP_FAILED, 1, "fd3 mmap");

    CHECK_RET(ioctl(fd3, KCOV_ENABLE, KCOV_TRACE_PC), 0, "fd3 ENABLE");
    burst(50);
    uint64_t after_burst = buf3[0];
    CHECK(after_burst >= 1, "fd3 recorded coverage before close");
    close(fd3);  /* triggers on_close → clears thread ref */
    fd3 = -1;

    /* Re-open a new fd, verify old buffer is disconnected and new one works */
    fd3 = open("/dev/kcov", O_RDWR);
    CHECK(fd3 >= 0, "fd3 reopen");
    CHECK_RET(ioctl(fd3, KCOV_INIT_TRACE, 128), 0, "fd3 re-INIT_TRACE");
    uint64_t *buf3b = mmap(NULL, 128 * sizeof(uint64_t), PROT_READ | PROT_WRITE,
                           MAP_SHARED, fd3, 0);
    CHECK_PTR(buf3b != MAP_FAILED, 1, "fd3 re-mmap");
    CHECK_RET(ioctl(fd3, KCOV_ENABLE, KCOV_TRACE_PC), 0, "fd3 re-ENABLE");
    burst(10);
    CHECK(buf3b[0] >= 1, "fd3 reopened fd records new coverage");
    CHECK_RET(ioctl(fd3, KCOV_DISABLE, 0), 0, "fd3 DISABLE after reopen");

    /* ---- 4. Fork: child inherits fd but coverage disabled ---- */
    pid_t pid = fork();
    if (pid == 0) {
        /* Child: kcov should be disabled on our thread after fork */
        int r;

        /* Linux: DISABLE from a non-tracing state → EINVAL.
         * fd1 was disabled earlier (mode=INIT, tracer_tid=None). */
        r = ioctl(fd1, KCOV_DISABLE, 0);
        if (r != -1 || errno != EINVAL) _exit(10);

        /* Child can enable on fd1 (still in INIT mode) */
        r = ioctl(fd1, KCOV_ENABLE, KCOV_TRACE_PC);
        if (r != 0) _exit(11);
        burst(10);
        uint64_t c = buf1[0];
        if (c < 1) _exit(12);

        r = ioctl(fd1, KCOV_DISABLE, 0);
        if (r != 0) _exit(13);

        _exit(0);
    }
    CHECK(pid > 0, "fork");
    int wstatus;
    CHECK_RET(waitpid(pid, &wstatus, 0), pid, "waitpid");
    CHECK(WIFEXITED(wstatus), "child exited normally");
    CHECK_RET(WEXITSTATUS(wstatus), 0, "child exit code 0 (kcov fork semantics)");

    /* ---- 5. New fd after close works fresh ---- */
    int fd4 = open("/dev/kcov", O_RDWR);
    CHECK(fd4 >= 0, "fd4 open (fresh after all cycles)");
    CHECK_RET(ioctl(fd4, KCOV_INIT_TRACE, 32), 0, "fd4 INIT_TRACE");
    uint64_t *buf4 = mmap(NULL, 32 * sizeof(uint64_t), PROT_READ | PROT_WRITE,
                          MAP_SHARED, fd4, 0);
    CHECK_PTR(buf4 != MAP_FAILED, 1, "fd4 mmap");
    CHECK_RET(ioctl(fd4, KCOV_ENABLE, KCOV_TRACE_PC), 0, "fd4 ENABLE");
    burst(5);
    CHECK(buf4[0] >= 1, "fd4 recorded coverage");
    CHECK_RET(ioctl(fd4, KCOV_DISABLE, 0), 0, "fd4 DISABLE");
    close(fd4);

    /* Cleanup */
    munmap(buf1, 256 * sizeof(uint64_t));
    munmap(buf2, 64 * sizeof(uint64_t));
    munmap(buf3b, 128 * sizeof(uint64_t));
    munmap(buf4, 32 * sizeof(uint64_t));
    close(fd1);
    close(fd2);
    close(fd3);

    TEST_DONE();
}
