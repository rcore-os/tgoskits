// Regression tests for the Linux-compat foundational kernel fixes in this PR
// (#1114). Each block exercises one fix so a future revert FAILs here:
//   1. procfs  /proc/sys/kernel/{osrelease,ostype}            (feature detection)
//   2. MemoryFs statfs reports 4 GiB                           (Java-server usable-space gate)
//   3. non-FIXED mmap placement kept below the stack guard     (#242 V8 pointer-cage)
//   4. MADV_DONTNEED/MADV_FREE frame reclaim + range ENOMEM    (#259 + madvise range check)
//   5. fcntl F_GET/SET_RW_HINT ABI (write-back / EINVAL / EFAULT)
//
// The axfs-ng EOF partial-page tail-zero fix is covered by its own test in
// PR #1164 (syscall-test-mmap-populate-eof) and is intentionally not duplicated
// here. riscv_hwprobe is a riscv64-only ENOSYS-avoidance stub (no portable
// userspace assertion) and is exercised by the riscv64 boot path.

#include "test_framework.h"

#include <stdint.h>
#include <unistd.h>
#include <fcntl.h>
#include <sys/mman.h>
#include <sys/statfs.h>

#define TMPFS_MAGIC 0x01021994
#define F_GET_RW_HINT_ 1035
#define F_SET_RW_HINT_ 1036

int main(void)
{
    TEST_START("foundational Linux-compat fixes (#1114)");

    // ---- 1. procfs osrelease / ostype ----
    char buf[64];
    int fd = open("/proc/sys/kernel/osrelease", O_RDONLY);
    CHECK(fd >= 0, "open /proc/sys/kernel/osrelease");
    if (fd >= 0) {
        ssize_t n = read(fd, buf, sizeof(buf) - 1);
        CHECK(n > 0, "read /proc/sys/kernel/osrelease");
        if (n > 0) {
            buf[(size_t)n] = '\0';
            CHECK(strstr(buf, "6.6") != NULL, "osrelease reports a 6.6 kernel (modern feature paths)");
        }
        close(fd);
    }
    fd = open("/proc/sys/kernel/ostype", O_RDONLY);
    CHECK(fd >= 0, "open /proc/sys/kernel/ostype");
    if (fd >= 0) {
        ssize_t n = read(fd, buf, sizeof(buf) - 1);
        if (n > 0) {
            buf[(size_t)n] = '\0';
        }
        CHECK(n > 0 && strncmp(buf, "Linux", 5) == 0, "ostype is \"Linux\"");
        close(fd);
    }

    // ---- 2. MemoryFs statfs reports 4 GiB ----
    struct statfs sf;
    const char *tp = NULL;
    if (statfs("/tmp", &sf) == 0 && (unsigned long)sf.f_type == (unsigned long)TMPFS_MAGIC) {
        tp = "/tmp";
    } else if (statfs("/dev/shm", &sf) == 0 && (unsigned long)sf.f_type == (unsigned long)TMPFS_MAGIC) {
        tp = "/dev/shm";
    }
    CHECK(tp != NULL, "found a tmpfs mount (TMPFS_MAGIC) at /tmp or /dev/shm");
    if (tp != NULL) {
        unsigned long long total = (unsigned long long)sf.f_bsize * (unsigned long long)sf.f_blocks;
        CHECK(total >= (4ULL << 30), "tmpfs statfs reports >= 4 GiB total (BookKeeper/RocksDB usable-space gate)");
    }

    // ---- 3. non-FIXED mmap placement stays below the stack guard (#242) ----
    volatile char stackvar = 0;
    (void)stackvar;
    size_t cage = (size_t)256 << 20; // 256 MiB PROT_NONE reservation, V8-cage style
    void *p = mmap(NULL, cage, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(p != MAP_FAILED, "large PROT_NONE non-FIXED reservation (V8 cage style) succeeds");
    if (p != MAP_FAILED) {
        uintptr_t top = (uintptr_t)p + cage;
        CHECK(top <= (uintptr_t)&stackvar,
              "#242: non-FIXED reservation placed below the user stack, not in the guard slot above it");
        munmap(p, cage);
    }

    // ---- 4. MADV_DONTNEED / MADV_FREE reclaim + range ENOMEM ----
    long pg = sysconf(_SC_PAGESIZE);
    if (pg <= 0) {
        pg = 4096;
    }
    char *a = mmap(NULL, (size_t)pg, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(a != MAP_FAILED, "anon mmap 1 page for MADV_DONTNEED");
    if (a != MAP_FAILED) {
        memset(a, 0xAB, (size_t)pg);
        CHECK((unsigned char)a[0] == 0xAB, "page written 0xAB before MADV_DONTNEED");
        CHECK_RET(madvise(a, (size_t)pg, MADV_DONTNEED), 0, "madvise(MADV_DONTNEED) on mapped anon page");
        CHECK((unsigned char)a[0] == 0, "#259: page reads back zero after MADV_DONTNEED (re-faulted fresh)");
        munmap(a, (size_t)pg);
    }
    char *f = mmap(NULL, (size_t)pg, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(f != MAP_FAILED, "anon mmap 1 page for MADV_FREE");
    if (f != MAP_FAILED) {
        memset(f, 0xCD, (size_t)pg);
        CHECK_RET(madvise(f, (size_t)pg, MADV_FREE), 0, "madvise(MADV_FREE) accepted on mapped anon page");
        munmap(f, (size_t)pg);
    }
    // man 2 madvise ENOMEM: the WHOLE range must be mapped — a hole must fail.
    char *b = mmap(NULL, (size_t)pg * 3, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(b != MAP_FAILED, "anon mmap 3 pages for the hole test");
    if (b != MAP_FAILED) {
        CHECK_RET(munmap(b + pg, (size_t)pg), 0, "punch a hole (munmap the middle page)");
        CHECK_ERR(madvise(b, (size_t)pg * 3, MADV_DONTNEED), ENOMEM,
                  "madvise over [mapped][hole][mapped] returns ENOMEM");
        munmap(b, (size_t)pg);
        munmap(b + pg * 2, (size_t)pg);
    }

    // ---- 5. fcntl F_GET/SET_RW_HINT ABI ----
    fd = open("/tmp/rwhint-1114.tmp", O_RDWR | O_CREAT | O_TRUNC, 0644);
    CHECK(fd >= 0, "open temp file for F_*_RW_HINT");
    if (fd >= 0) {
        uint64_t hint = 0xdeadbeef;
        CHECK_RET(fcntl(fd, F_GET_RW_HINT_, &hint), 0, "F_GET_RW_HINT returns 0");
        CHECK(hint == 0, "F_GET_RW_HINT writes back RWH_WRITE_LIFE_NOT_SET (0)");
        uint64_t valid = 3; // RWH_WRITE_LIFE_MEDIUM
        CHECK_RET(fcntl(fd, F_SET_RW_HINT_, &valid), 0, "F_SET_RW_HINT accepts a valid hint");
        uint64_t bad = 99;
        CHECK_ERR(fcntl(fd, F_SET_RW_HINT_, &bad), EINVAL, "F_SET_RW_HINT rejects out-of-range hint with EINVAL");
        CHECK_ERR(fcntl(fd, F_GET_RW_HINT_, NULL), EFAULT, "F_GET_RW_HINT with a NULL pointer returns EFAULT");
        close(fd);
        unlink("/tmp/rwhint-1114.tmp");
    }

    TEST_DONE();
}
