// Correctness guard for the lock-free user-copy fast path
// (`starry-kernel/user-access-fastpath`, the consumer of ax-cpu
// `user_access_ok_page`). The fast path skips the aspace lock only when every
// covered page is already present with the requested EL0 permission; it must
// otherwise fall through to the unchanged locked slow path. The subtle case is
// a write: a copy-on-write page is present but read-only, so the write probe
// must MISS and let the slow path perform the COW copy. A third case covers a
// moved-away address after an `mremap` move: `mremap` clears the source PTEs
// (and flushes the TLB) before it drops the source VMA, so there is never a
// "stale PTE, no VMA" window; the fast path must fault on the old address just
// like the slow path and must not treat an absent old PTE as accessible. These
// cases run through the real syscall user-copy path, so with the fast path
// enabled they exercise it directly.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

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

// A present, EL0-writable page: the fast path's write probe hits, so the kernel
// writes through it without the aspace lock. Uses getcwd (a copy_to_user).
static void present_page_fastpath_hit(void)
{
    char *buf = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                     MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (buf == MAP_FAILED) {
        note_fail("mmap present page", strerror(errno));
        return;
    }
    buf[0] = 'x'; // fault in -> present + writable

    if (chdir("/") != 0) {
        note_fail("chdir /", strerror(errno));
        munmap(buf, 4096);
        return;
    }
    errno = 0;
    char *ret = getcwd(buf, 4096);
    if (ret == buf && strcmp(buf, "/") == 0) {
        note_pass("present writable page: syscall write hits fast path");
    } else {
        char detail[128];
        snprintf(detail, sizeof(detail), "ret=%p errno=%d (%s) buf='%s'",
                 (void *)ret, errno, strerror(errno), buf);
        note_fail("present writable page write", detail);
    }
    munmap(buf, 4096);
}

// A copy-on-write page is present but read-only, so the fast path's write probe
// must miss and route to the slow path, which performs the COW copy. A child
// reads a payload from a pipe into its COW page (a kernel write via the
// user-copy path); the write must succeed and stay private to the child.
static void cow_page_write_via_syscall(void)
{
    int pfd[2];
    if (pipe(pfd) != 0) {
        note_fail("pipe", strerror(errno));
        return;
    }

    char *page = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (page == MAP_FAILED) {
        note_fail("mmap cow page", strerror(errno));
        return;
    }
    page[0] = 'A'; // fault in; fork() below marks this page COW read-only

    static const char payload[] = "COW-WRITE-OK";
    pid_t pid = fork();
    if (pid < 0) {
        note_fail("fork", strerror(errno));
        munmap(page, 4096);
        return;
    }
    if (pid == 0) {
        // Child: `page` is a present, read-only COW page shared with the parent.
        // read() has the kernel write the payload into it via the user-copy
        // path -> fast-path write probe misses (read-only) -> slow path
        // COW-copies the page so the write lands.
        close(pfd[1]);
        size_t got = 0;
        while (got < sizeof(payload)) {
            ssize_t n = read(pfd[0], page + got, sizeof(payload) - got);
            if (n <= 0) {
                _exit(2);
            }
            got += (size_t)n;
        }
        _exit(memcmp(page, payload, sizeof(payload)) == 0 ? 0 : 3);
    }

    // Parent: feed the payload, wait, verify the child's write succeeded and our
    // copy is untouched (COW isolation).
    close(pfd[0]);
    if (write(pfd[1], payload, sizeof(payload)) != (ssize_t)sizeof(payload)) {
        note_fail("parent write to pipe", strerror(errno));
    }
    close(pfd[1]);

    int st = 0;
    waitpid(pid, &st, 0);
    if (WIFEXITED(st) && WEXITSTATUS(st) == 0) {
        note_pass("COW page written via read(): fast path misses -> slow-path COW");
    } else {
        char detail[96];
        snprintf(detail, sizeof(detail), "child status=%d", st);
        note_fail("COW page write via syscall", detail);
    }
    if (page[0] == 'A') {
        note_pass("parent copy preserved (COW isolation)");
    } else {
        char detail[64];
        snprintf(detail, sizeof(detail), "page[0]=%d expected 'A'", page[0]);
        note_fail("COW isolation", detail);
    }
    munmap(page, 4096);
}

// After an `mremap` move, the old address has no live PTE: mremap relocates the
// source page-table entries (clearing each and flushing the TLB) before it drops
// the source VMA metadata. A syscall user-copy targeting the moved-away old
// address must therefore fault on the AT-probe fast path exactly as on the
// locked slow path -- the fast path must not treat the absent old PTE as
// accessible. read() copies into the buffer (the same copy_to_user path the COW
// case uses); it must return EFAULT.
static void mremap_moved_page_faults_on_old_address(void)
{
    long ps = sysconf(_SC_PAGESIZE);
    if (ps <= 0) {
        note_fail("sysconf(_SC_PAGESIZE)", strerror(errno));
        return;
    }
    size_t page_size = (size_t)ps;

    // Reserve a distinct destination so MREMAP_FIXED performs a real move
    // (old != new), guaranteeing the source PTEs are relocated and cleared.
    char *dst = mmap(NULL, page_size, PROT_READ | PROT_WRITE,
                     MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    char *src = mmap(NULL, page_size, PROT_READ | PROT_WRITE,
                     MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (dst == MAP_FAILED || src == MAP_FAILED) {
        note_fail("mmap mremap pages", strerror(errno));
        return;
    }
    src[0] = 'Z'; // present + writable before the move

    void *moved = mremap(src, page_size, page_size,
                         MREMAP_MAYMOVE | MREMAP_FIXED, dst);
    if (moved == MAP_FAILED) {
        note_fail("mremap move", strerror(errno));
        munmap(src, page_size);
        munmap(dst, page_size);
        return;
    }
    if (moved != dst) {
        note_fail("mremap move target", "moved != dst");
        munmap(dst, page_size);
        return;
    }
    // The data relocated -> the source PTE was cleared by the move, not left
    // stale at the old address.
    if (dst[0] != 'Z') {
        note_fail("mremap data relocation", "dst[0] != 'Z'");
        munmap(dst, page_size);
        return;
    }

    // `src` is now a moved-away address with no VMA and no PTE. Have the kernel
    // copy INTO it via read() and require EFAULT: the fast-path write probe
    // misses on the absent PTE, and the slow path rejects the missing VMA.
    int pfd[2];
    if (pipe(pfd) != 0) {
        note_fail("mremap pipe", strerror(errno));
        munmap(dst, page_size);
        return;
    }
    if (write(pfd[1], "x", 1) != 1) {
        note_fail("mremap pipe write", strerror(errno));
        close(pfd[0]);
        close(pfd[1]);
        munmap(dst, page_size);
        return;
    }
    errno = 0;
    ssize_t n = read(pfd[0], src, 1);
    if (n == -1 && errno == EFAULT) {
        note_pass("mremap moved page: user-copy to old address faults");
    } else {
        char detail[96];
        snprintf(detail, sizeof(detail), "n=%zd errno=%d (%s)", n, errno,
                 strerror(errno));
        note_fail("mremap moved page fault", detail);
    }
    close(pfd[0]);
    close(pfd[1]);
    munmap(dst, page_size);
}

int main(void)
{
    printf("=== usercopy-fastpath-cow-write ===\n");

    present_page_fastpath_hit();
    cow_page_write_via_syscall();
    mremap_moved_page_faults_on_old_address();

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("ALL TESTS PASSED\n");
        return 0;
    }
    printf("SOME TESTS FAILED\n");
    return 1;
}
