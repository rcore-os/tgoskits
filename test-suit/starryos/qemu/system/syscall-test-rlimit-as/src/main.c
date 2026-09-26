/*
 * RLIMIT_AS on mmap(2) and mremap(2), following Linux may_expand_vm(): a
 * mapping fails with ENOMEM once the address space would exceed the limit.
 * Pages a fixed target already maps are released first, MREMAP_DONTUNMAP is
 * charged the whole new mapping, and RLIM_INFINITY disables the limit.
 * The at-limit expectations were recorded on Linux 6.6.
 */
#define _GNU_SOURCE
#include "test_framework.h"
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <sys/ipc.h>
#include <sys/resource.h>
#include <sys/shm.h>
#include <sys/syscall.h>
#include <unistd.h>

#define MB (1024UL * 1024UL)
#define ANON (MAP_PRIVATE | MAP_ANONYMOUS)

static size_t vmsize(void)
{
    char buf[128] = {0};
    int fd = open("/proc/self/statm", O_RDONLY);
    if (fd < 0)
        return 0;
    ssize_t n = read(fd, buf, sizeof buf - 1);
    close(fd);
    if (n <= 0)
        return 0;
    return strtoul(buf, NULL, 10) * (size_t)sysconf(_SC_PAGESIZE);
}

static int set_as(rlim_t cur)
{
    struct rlimit rl = {.rlim_cur = cur, .rlim_max = RLIM_INFINITY};
    return setrlimit(RLIMIT_AS, &rl);
}

static int unlimited(void)
{
    return set_as(RLIM_INFINITY);
}

/* Lowers the limit to the current address space size. */
static int at_limit(void)
{
    size_t vm = vmsize();
    return vm ? set_as((rlim_t)vm) : -1;
}

static void *anon(size_t len)
{
    return mmap(NULL, len, PROT_READ | PROT_WRITE, ANON, -1, 0);
}

int main(void)
{
    TEST_START("RLIMIT_AS on mmap and mremap");

    struct rlimit def;
    CHECK_RET(getrlimit(RLIMIT_AS, &def), 0, "getrlimit(RLIMIT_AS)");
    CHECK(def.rlim_cur == RLIM_INFINITY && def.rlim_max == RLIM_INFINITY,
          "default RLIMIT_AS is RLIM_INFINITY");
    CHECK(vmsize() > 0, "/proc/self/statm reports the address space size");

    /* A single mapping larger than the whole limit fails regardless of usage. */
    CHECK_RET(set_as(16 * MB), 0, "setrlimit(RLIMIT_AS, 16M)");
    void *p = mmap(NULL, 512 * MB, PROT_NONE, ANON, -1, 0);
    CHECK(p == MAP_FAILED && errno == ENOMEM, "mmap 512M under a 16M limit fails ENOMEM");

    CHECK_RET(unlimited(), 0, "setrlimit(RLIMIT_AS, RLIM_INFINITY)");
    p = mmap(NULL, 512 * MB, PROT_NONE, ANON, -1, 0);
    CHECK(p != MAP_FAILED, "mmap 512M succeeds under RLIM_INFINITY");
    if (p != MAP_FAILED)
        munmap(p, 512 * MB);

    /* mmap MAP_FIXED over an equal mapping adds nothing. */
    char *m = mmap(NULL, 8 * MB, PROT_READ, ANON, -1, 0);
    CHECK(m != MAP_FAILED, "mmap 8M");
    CHECK_RET(at_limit(), 0, "limit = address space size");
    p = mmap(m, 8 * MB, PROT_READ, ANON | MAP_FIXED, -1, 0);
    CHECK(p == m, "at the limit, MAP_FIXED 8M over an 8M mapping succeeds");
    CHECK_RET(unlimited(), 0, "lift the limit");
    munmap(m, 8 * MB);

    /* mmap MAP_FIXED that grows past the mapping it replaces. */
    char *g = mmap(NULL, 16 * MB, PROT_READ, ANON, -1, 0);
    CHECK(g != MAP_FAILED, "mmap 16M");
    munmap(g + 8 * MB, 8 * MB);
    CHECK_RET(at_limit(), 0, "limit = address space size");
    p = mmap(g, 16 * MB, PROT_READ, ANON | MAP_FIXED, -1, 0);
    CHECK(p == MAP_FAILED && errno == ENOMEM,
          "at the limit, MAP_FIXED 16M over an 8M mapping fails ENOMEM");
    CHECK_RET(unlimited(), 0, "lift the limit");
    munmap(g, 16 * MB);

    /* mremap growth is charged its delta. */
    char *base = anon(4 * MB);
    CHECK(base != MAP_FAILED, "mmap 4M");
    CHECK_RET(at_limit(), 0, "limit = address space size");
    p = mremap(base, 4 * MB, 6 * MB, MREMAP_MAYMOVE);
    CHECK(p == MAP_FAILED && errno == ENOMEM, "at the limit, mremap 4M->6M fails ENOMEM");
    CHECK_RET(unlimited(), 0, "lift the limit");
    p = mremap(base, 4 * MB, 6 * MB, MREMAP_MAYMOVE);
    CHECK(p != MAP_FAILED, "mremap 4M->6M succeeds under RLIM_INFINITY");
    munmap(p != MAP_FAILED ? p : base, p != MAP_FAILED ? 6 * MB : 4 * MB);

    /* mremap growth onto a fixed target that already maps more than the delta. */
    char *src = anon(4 * MB);
    char *dst = anon(6 * MB);
    CHECK(src != MAP_FAILED && dst != MAP_FAILED, "mmap 4M source and 6M target");
    CHECK_RET(at_limit(), 0, "limit = address space size");
    p = mremap(src, 4 * MB, 6 * MB, MREMAP_MAYMOVE | MREMAP_FIXED, dst);
    CHECK(p == dst, "at the limit, mremap 4M->6M onto a mapped 6M target succeeds");
    CHECK_RET(unlimited(), 0, "lift the limit");
    munmap(dst, 6 * MB);

    /* Growing the heap is charged too: Linux do_brk_flags() calls
     * may_expand_vm(). musl's sbrk() only answers sbrk(0), so ask brk
     * directly; on failure it returns the old break. */
    long top = syscall(SYS_brk, 0);
    CHECK(top > 0, "read the program break");
    CHECK_RET(at_limit(), 0, "limit = address space size");
    CHECK_RET(syscall(SYS_brk, top + (long)(4 * MB)), top,
              "at the limit, growing the heap by 4M leaves the break where it was");
    CHECK_RET(unlimited(), 0, "lift the limit");
    CHECK_RET(syscall(SYS_brk, top + (long)(4 * MB)), top + (long)(4 * MB),
              "the heap grows once the limit is lifted");
    CHECK_RET(syscall(SYS_brk, top), top, "shrink the heap back");

    /* shmat() maps through do_mmap(), which charges the whole attachment. */
    int seg = shmget(IPC_PRIVATE, 4 * MB, IPC_CREAT | 0600);
    CHECK(seg >= 0, "create a 4M shared memory segment");
    if (seg >= 0) {
        CHECK_RET(at_limit(), 0, "limit = address space size");
        void *at = shmat(seg, NULL, 0);
        CHECK(at == (void *)-1 && errno == ENOMEM, "at the limit, attaching 4M fails ENOMEM");
        CHECK_RET(unlimited(), 0, "lift the limit");
        at = shmat(seg, NULL, 0);
        CHECK(at != (void *)-1, "attaching succeeds under RLIM_INFINITY");
        if (at != (void *)-1)
            shmdt(at);
        shmctl(seg, IPC_RMID, NULL);
    }

    /* MREMAP_DONTUNMAP keeps the source, so the new mapping is charged in full. */
    char *keep = anon(4 * MB);
    CHECK(keep != MAP_FAILED, "mmap 4M");
    CHECK_RET(at_limit(), 0, "limit = address space size");
    p = mremap(keep, 4 * MB, 4 * MB, MREMAP_MAYMOVE | MREMAP_DONTUNMAP);
    CHECK(p == MAP_FAILED && errno == ENOMEM,
          "at the limit, same-size DONTUNMAP into free space fails ENOMEM");
    CHECK_RET(unlimited(), 0, "lift the limit");
    p = mremap(keep, 4 * MB, 4 * MB, MREMAP_MAYMOVE | MREMAP_DONTUNMAP);
    CHECK(p != MAP_FAILED, "same-size DONTUNMAP succeeds under RLIM_INFINITY");
    if (p != MAP_FAILED)
        munmap(p, 4 * MB);

    /* DONTUNMAP onto a fixed target of equal size releases that target first. */
    char *target = anon(4 * MB);
    CHECK(target != MAP_FAILED, "mmap 4M target");
    CHECK_RET(at_limit(), 0, "limit = address space size");
    p = mremap(keep, 4 * MB, 4 * MB, MREMAP_MAYMOVE | MREMAP_FIXED | MREMAP_DONTUNMAP, target);
    CHECK(p == target, "at the limit, DONTUNMAP onto a mapped 4M target succeeds");
    CHECK_RET(unlimited(), 0, "lift the limit");
    munmap(target, 4 * MB);
    munmap(keep, 4 * MB);

    /* An occupied MAP_FIXED_NOREPLACE target fails EEXIST before the limit is consulted. */
    char *held = anon(4 * MB);
    CHECK(held != MAP_FAILED, "mmap 4M");
    CHECK_RET(at_limit(), 0, "limit = address space size");
    p = mmap(NULL, 64 * MB, PROT_READ, ANON, -1, 0);
    CHECK(p == MAP_FAILED && errno == ENOMEM, "at the limit, a new 64M mapping fails ENOMEM");
    p = mmap(held, 64 * MB, PROT_READ, ANON | MAP_FIXED_NOREPLACE, -1, 0);
    CHECK(p == MAP_FAILED && errno == EEXIST,
          "at the limit, MAP_FIXED_NOREPLACE 64M over a mapped 4M fails EEXIST");
    CHECK_RET(unlimited(), 0, "lift the limit");
    munmap(held, 4 * MB);

    TEST_DONE();
}
