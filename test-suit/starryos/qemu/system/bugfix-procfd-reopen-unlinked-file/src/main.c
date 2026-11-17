#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <poll.h>
#include <sched.h>
#include <sys/mount.h>
#include <sys/wait.h>
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

static int raw_open(int openat2, int dirfd, const char *path, int flags)
{
    struct raw_open_how how = {.flags = (uint64_t)flags};
    return openat2 ? syscall(SYS_openat2, dirfd, path, &how, sizeof(how))
                   : syscall(SYS_openat, dirfd, path, flags, 0);
}

static int reject_open(int dirfd, const char *path, int flags, int expected)
{
    int failed = 0;
    for (int openat2 = 0; openat2 < 2; ++openat2) {
        errno = 0;
        int fd = raw_open(openat2, dirfd, path, flags | O_CLOEXEC);
        int saved = errno;
        if (fd >= 0) close(fd);
        if (fd != -1 || saved != expected) {
            printf("FAIL: openat%s %s flags=%#x result=%d errno=%d expected=%d\n",
                   openat2 ? "2" : "", path, flags, fd, saved, expected);
            failed = 1;
        }
    }
    return failed;
}

/* A retained proc directory stays usable, but an absolute spelling must see
 * the new mount. This distinguishes object ownership from a path shortcut. */
static int check_masked_links(int procdir, int original)
{
    const char *prefixes[] = {"/proc/self/fd/", "/dev/fd/"};
    int failed = 0;
    for (unsigned i = 0; i < sizeof(prefixes) / sizeof(prefixes[0]); ++i) {
        pid_t child = fork();
        if (child < 0) return fail("fork masked proc lookup");
        if (child == 0) {
            if (unshare(CLONE_NEWNS) != 0 ||
                mount(NULL, "/", NULL, MS_PRIVATE | MS_REC, NULL) != 0 ||
                mount("tmpfs", "/proc", "tmpfs", MS_NOSUID | MS_NODEV, NULL) != 0)
            {
                fail("SETUP cover proc mount");
                _exit(125);
            }
            char path[64], slot[16];
            snprintf(path, sizeof(path), "%s%d", prefixes[i], original);
            snprintf(slot, sizeof(slot), "%d", original);
            int bad = reject_open(AT_FDCWD, path, O_RDONLY | O_NONBLOCK, ENOENT);
            for (int openat2 = 0; openat2 < 2; ++openat2) {
                int fd = raw_open(openat2, procdir, slot, O_RDONLY | O_NONBLOCK | O_CLOEXEC);
                struct stat before, after;
                if (fd < 0 || fstat(original, &before) != 0 || fstat(fd, &after) != 0 ||
                    before.st_dev != after.st_dev || before.st_ino != after.st_ino)
                    bad |= fail("retained proc directory still opens the same object");
                if (fd >= 0) close(fd);
            }
            _exit(bad);
        }
        int status = 0;
        if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0)
            failed |= fail("masked absolute proc link must reject without invalidating retained dirfd");
    }
    return failed;
}

static int check_pipe_route(int dirfd, const char *path, int original)
{
    int failed = reject_open(dirfd, path, O_RDONLY | O_NOFOLLOW, ELOOP)
               | reject_open(dirfd, path, O_RDONLY | O_CREAT | O_EXCL, EEXIST)
               | reject_open(dirfd, path, O_RDONLY | O_DIRECTORY, ENOTDIR);
    for (int access = O_RDONLY; access <= O_RDWR; ++access) {
        for (int openat2 = 0; openat2 < 2; ++openat2) {
            int fd = raw_open(openat2, dirfd, path, access | O_NONBLOCK | O_CLOEXEC);
            struct stat before, after;
            if (fd < 0 || fstat(original, &before) != 0 || fstat(fd, &after) != 0 ||
                !S_ISFIFO(after.st_mode) || before.st_dev != after.st_dev ||
                before.st_ino != after.st_ino ||
                (fcntl(fd, F_GETFL) & O_ACCMODE) != access ||
                (fcntl(fd, F_GETFD) & FD_CLOEXEC) == 0 ||
                fcntl(fd, F_SETFL, access) != 0 ||
                (fcntl(original, F_GETFL) & O_NONBLOCK) == 0) {
                printf("FAIL: pipe route %s access=%d openat2=%d errno=%d\n",
                       path, access, openat2, errno);
                failed = 1;
            }
            if (fd >= 0) close(fd);
        }
    }
    return failed;
}

static int check_pipe_reopen(int procdir, const char *alias)
{
    int endpoints[2] = {-1, -1};
    int reader = -1, writer = -1, extra = -1, failed = 0;
    if (pipe2(endpoints, O_CLOEXEC | O_NONBLOCK) != 0)
        return fail("SETUP anonymous pipe");
    for (int end = 0; end < 2; ++end) {
        char absolute[64], relative[16], aliased[160];
        snprintf(absolute, sizeof(absolute), "/proc/self/fd/%d", endpoints[end]);
        snprintf(relative, sizeof(relative), "%d", endpoints[end]);
        snprintf(aliased, sizeof(aliased), "%s/%d", alias, endpoints[end]);
        failed |= check_pipe_route(AT_FDCWD, absolute, endpoints[end]);
        failed |= check_pipe_route(procdir, relative, endpoints[end]);
        failed |= check_pipe_route(AT_FDCWD, aliased, endpoints[end]);
    }
    failed |= check_masked_links(procdir, endpoints[0]);
    char path[64];
    snprintf(path, sizeof(path), "/proc/self/fd/%d", endpoints[1]);
    reader = raw_open(0, AT_FDCWD, path, O_RDONLY | O_NONBLOCK | O_CLOEXEC);
    snprintf(path, sizeof(path), "/proc/self/fd/%d", endpoints[0]);
    writer = raw_open(1, AT_FDCWD, path, O_WRONLY | O_NONBLOCK | O_CLOEXEC);
    if (reader < 0 || writer < 0) {
        failed |= fail("reopen opposite anonymous pipe access modes");
        goto cleanup;
    }
    close(endpoints[0]); endpoints[0] = -1;
    close(endpoints[1]); endpoints[1] = -1;
    char byte = 0;
    if (write(writer, "P", 1) != 1 || read(reader, &byte, 1) != 1 || byte != 'P')
        failed |= fail("reopened descriptions keep the original shared pipe buffer");
    close(writer); writer = -1;
    snprintf(path, sizeof(path), "/proc/self/fd/%d", reader);
    extra = raw_open(0, AT_FDCWD, path, O_RDONLY | O_NONBLOCK | O_CLOEXEC);
    struct pollfd pollfd = {.fd = extra, .events = POLLIN};
    if (extra < 0 || poll(&pollfd, 1, 0) != 1 || !(pollfd.revents & POLLHUP))
        failed |= fail("anonymous reopen with no writers immediately observes HUP");
    if (extra >= 0) close(extra);
    extra = -1;
    writer = raw_open(0, AT_FDCWD, path, O_WRONLY | O_NONBLOCK | O_CLOEXEC);
    if (writer < 0) {
        failed |= fail("create writer while a reader remains");
        goto cleanup;
    }
    close(reader); reader = -1;
    snprintf(path, sizeof(path), "/proc/self/fd/%d", writer);
    extra = raw_open(1, AT_FDCWD, path, O_WRONLY | O_NONBLOCK | O_CLOEXEC);
    pollfd = (struct pollfd){.fd = extra, .events = POLLOUT};
    if (extra < 0 || poll(&pollfd, 1, 0) != 1 || !(pollfd.revents & POLLERR))
        failed |= fail("anonymous reopen needs no reader and observes broken-pipe readiness");
cleanup:
    if (extra >= 0) close(extra);
    if (reader >= 0) close(reader);
    if (writer >= 0) close(writer);
    if (endpoints[0] >= 0) close(endpoints[0]);
    if (endpoints[1] >= 0) close(endpoints[1]);
    return failed;
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

    if (reject_open(dirfd, path, O_RDONLY | O_CREAT | O_EXCL, EEXIST) ||
        reject_open(dirfd, path, O_RDONLY | O_NOFOLLOW, ELOOP))
        return 1;

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
    int failed = 1, supplemental_failed = 0;
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

    supplemental_failed |= check_masked_links(procdir, pathfd);
    supplemental_failed |= check_pipe_reopen(procdir, alias);

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
    failed = supplemental_failed;
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
        puts("PASS: procfd object identity, independent descriptions, mount visibility and slot reuse");
    return failed;
}
