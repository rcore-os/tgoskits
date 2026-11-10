#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SYS_openat2
#define SYS_openat2 437
#endif

struct raw_open_how {
    uint64_t flags, mode, resolve;
};

static int fail(const char *stage)
{
    printf("FAIL: %s errno=%d (%s)\n", stage, errno, strerror(errno));
    return 1;
}

static int same_object(const struct stat *a, const struct stat *b)
{
    return a->st_dev == b->st_dev && a->st_ino == b->st_ino
           && S_ISREG(b->st_mode);
}

/* One fixture exercises lookup routes, not a mirror of the proc path parser.
 * Each reopen must allocate a new open file description, even after unlink. */
static int check_link(int dirfd, const char *path, const char *text,
                      int original)
{
    struct stat target, followed, link, pinned;
    char buf[256];
    ssize_t n = syscall(SYS_readlinkat, dirfd, path, buf, sizeof(buf));
    if (n != (ssize_t)strlen(text) || memcmp(buf, text, strlen(text)) != 0)
        return fail("readlinkat describes the pinned object");
    /* fstatat is portable to asm-generic targets without SYS_stat. */
    if (fstat(original, &target) != 0 ||
        fstatat(dirfd, path, &followed, 0) != 0 ||
        !same_object(&target, &followed))
        return fail("fstatat follows procfd to the real file");
    if (fstatat(dirfd, path, &link, AT_SYMLINK_NOFOLLOW) != 0 ||
        !S_ISLNK(link.st_mode))
        return fail("fstatat nofollow observes the proc link");

    int linkfd = syscall(SYS_openat, dirfd, path,
                         O_PATH | O_NOFOLLOW | O_CLOEXEC, 0);
    int failed = linkfd < 0 || fstat(linkfd, &pinned) != 0 ||
                 !S_ISLNK(pinned.st_mode) || pinned.st_dev != link.st_dev ||
                 pinned.st_ino != link.st_ino;
    if (linkfd >= 0) close(linkfd);
    if (failed) return fail("O_PATH nofollow pins the link, not its target");

    if (lseek(original, 2, SEEK_SET) != 2)
        return fail("set original offset");
    struct raw_open_how how = {.flags = O_RDONLY | O_CLOEXEC};
    for (int use_openat2 = 0; use_openat2 < 2; ++use_openat2) {
        int reopened = use_openat2
            ? syscall(SYS_openat2, dirfd, path, &how, sizeof(how))
            : syscall(SYS_openat, dirfd, path, O_RDONLY | O_CLOEXEC, 0);
        char byte = 0;
        failed = reopened < 0 || fstat(reopened, &followed) != 0 ||
                 !same_object(&target, &followed) ||
                 read(reopened, &byte, 1) != 1 || byte != 'M' ||
                 lseek(original, 0, SEEK_CUR) != 2;
        if (reopened >= 0) close(reopened);
        if (failed) return fail(use_openat2
            ? "openat2 preserves identity with an independent offset"
            : "openat preserves identity with an independent offset");
    }
    return 0;
}

int main(void)
{
    char dir[] = "/tmp/procfd-object-XXXXXX";
    char file[128], renamed[128], alias[128], via_alias[160];
    char absolute[64], slot[16], deleted[160], retained_path[64];
    int original = -1, pathfd = -1, procdir = -1, retained = -1, replacement = -1;
    int failed = 1;
    setvbuf(stdout, NULL, _IONBF, 0);
    puts("STARRY_SYSTEM_TEST_BEGIN: bugfix-procfd-reopen-unlinked-file");
    if (mkdtemp(dir) == NULL) return fail("SETUP mkdtemp");
    snprintf(file, sizeof(file), "%s/file", dir);
    snprintf(renamed, sizeof(renamed), "%s/renamed", dir);
    snprintf(alias, sizeof(alias), "%s/proc-alias", dir);
    original = syscall(SYS_openat, AT_FDCWD, file,
                       O_RDWR | O_CREAT | O_EXCL | O_CLOEXEC, 0600);
    if (original < 0 || write(original, "Magic", 5) != 5 ||
        symlink("/proc/self/fd", alias) != 0) {
        fail("SETUP file and ordinary symlink alias");
        goto cleanup;
    }
    pathfd = syscall(SYS_openat, AT_FDCWD, file, O_PATH | O_CLOEXEC, 0);
    procdir = syscall(SYS_openat, AT_FDCWD, "/proc/self/fd",
                      O_PATH | O_DIRECTORY | O_CLOEXEC, 0);
    if (pathfd < 0 || procdir < 0) {
        fail("SETUP pin fixture and proc directory");
        goto cleanup;
    }
    snprintf(absolute, sizeof(absolute), "/proc/self/fd/%d", pathfd);
    snprintf(slot, sizeof(slot), "%d", pathfd);
    snprintf(via_alias, sizeof(via_alias), "%s/%d", alias, pathfd);

    for (int phase = 0; phase < 3; ++phase) {
        const char *text = file;
        if (phase == 1) {
            if (rename(file, renamed) != 0) {
                fail("rename pinned file");
                goto cleanup;
            }
            text = renamed;
        } else if (phase == 2) {
            if (unlink(renamed) != 0) {
                fail("unlink pinned file");
                goto cleanup;
            }
            snprintf(deleted, sizeof(deleted), "%s (deleted)", renamed);
            text = deleted;
        }
        if (check_link(AT_FDCWD, absolute, text, original) ||
            check_link(procdir, slot, text, original) ||
            check_link(AT_FDCWD, via_alias, text, original))
            goto cleanup;
    }

    /* Capture an object reference, then reuse exactly the old proc fd slot.
     * A new lookup must not retain the old cached target. */
    retained = syscall(SYS_openat, AT_FDCWD, absolute, O_PATH | O_CLOEXEC, 0);
    replacement = syscall(SYS_openat, AT_FDCWD, file,
                          O_RDWR | O_CREAT | O_EXCL | O_CLOEXEC, 0600);
    if (retained < 0 || replacement < 0 ||
        write(replacement, "New", 3) != 3) {
        fail("SETUP replacement object");
        goto cleanup;
    }
    int reused = pathfd;
    close(pathfd);
    pathfd = -1;
    if (syscall(SYS_dup3, replacement, reused, O_CLOEXEC) != reused) {
        fail("reuse proc fd slot");
        goto cleanup;
    }
    pathfd = reused;
    struct stat new_st, lookup_st, old_st, retained_st;
    if (fstat(replacement, &new_st) != 0 ||
        fstatat(procdir, slot, &lookup_st, 0) != 0 ||
        !same_object(&new_st, &lookup_st) ||
        fstat(original, &old_st) != 0 || fstat(retained, &retained_st) != 0 ||
        !same_object(&old_st, &retained_st) ||
        same_object(&old_st, &new_st)) {
        fail("new lookup sees reused slot; captured reference keeps old identity");
        goto cleanup;
    }
    snprintf(retained_path, sizeof(retained_path), "/proc/self/fd/%d", retained);
    if (check_link(AT_FDCWD, retained_path, deleted, original))
        goto cleanup;
    int newfd = syscall(SYS_openat, procdir, slot, O_RDONLY | O_CLOEXEC, 0);
    char byte = 0;
    int bad = newfd < 0 || read(newfd, &byte, 1) != 1 || byte != 'N';
    if (newfd >= 0) close(newfd);
    if (bad) {
        fail("reopen reused slot reads the new object");
        goto cleanup;
    }
    failed = 0;
cleanup:
    if (replacement >= 0) close(replacement);
    if (retained >= 0) close(retained);
    if (procdir >= 0) close(procdir);
    if (pathfd >= 0) close(pathfd);
    if (original >= 0) close(original);
    if (unlink(file) != 0 && errno != ENOENT) failed |= fail("clean file");
    if (unlink(renamed) != 0 && errno != ENOENT) failed |= fail("clean renamed");
    if (unlink(alias) != 0 && errno != ENOENT) failed |= fail("clean alias");
    if (rmdir(dir) != 0) failed |= fail("clean directory");
    if (!failed)
        puts("PASS: procfd object identity, independent reopen and slot reuse");
    return failed;
}
