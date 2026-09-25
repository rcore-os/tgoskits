/* Regression for openat2 RESOLVE_* path-walk constraints.
 *
 * Each restriction is asserted in both directions: paths that comply must
 * open, and paths that violate the sandbox must fail with the errno Linux
 * returns (EXDEV for BENEATH/NO_XDEV boundary violations, ELOOP for
 * NO_SYMLINKS/NO_MAGICLINKS).
 */
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

#define RESOLVE_NO_XDEV 0x01
#define RESOLVE_NO_MAGICLINKS 0x02
#define RESOLVE_NO_SYMLINKS 0x04
#define RESOLVE_BENEATH 0x08
#define RESOLVE_IN_ROOT 0x10
#define RESOLVE_CACHED 0x20

#ifndef SYS_openat2
#if defined(__x86_64__) || defined(__aarch64__) || defined(__riscv) ||         \
    defined(__loongarch64)
#define SYS_openat2 437
#else
#error "SYS_openat2 is unknown for this architecture"
#endif
#endif

struct open_how {
    uint64_t flags;
    uint64_t mode;
    uint64_t resolve;
};

static int failures;

#define CHECK(condition, message)                                            \
    do {                                                                     \
        if (condition) {                                                     \
            printf("PASS: %s\n", message);                                   \
        } else {                                                             \
            printf("FAIL: %s: errno=%d (%s)\n", message, errno,              \
                   strerror(errno));                                         \
            failures++;                                                      \
        }                                                                    \
    } while (0)

static int do_openat2(int dirfd, const char *path, uint64_t resolve,
                      uint64_t flags, uint64_t mode)
{
    const struct open_how how = {
        .flags = flags,
        .mode = mode,
        .resolve = resolve,
    };

    return (int)syscall(SYS_openat2, dirfd, path, &how, sizeof(how));
}

/* Opens or creates `path` under `resolve`, expecting success. */
static void expect_open(const char *name, int dirfd, const char *path,
                        uint64_t resolve, uint64_t flags)
{
    errno = 0;
    int fd = do_openat2(dirfd, path, resolve, flags, 0);
    if (fd >= 0) {
        close(fd);
        CHECK(1, name);
    } else {
        CHECK(0, name);
    }
}

static void expect_errno(const char *name, int dirfd, const char *path,
                         uint64_t resolve, uint64_t flags, int expected)
{
    errno = 0;
    int fd = do_openat2(dirfd, path, resolve, flags, 0);
    if (fd >= 0) {
        close(fd);
        errno = 0;
        CHECK(0, name);
        return;
    }
    int err = errno;
    CHECK(err == expected, name);
}

int main(void)
{
    const char *root = "/tmp/o2c";
    char sub[64];
    char rel[64];
    char abs[64];
    char pid_stat[64];

    snprintf(sub, sizeof(sub), "%s/sub", root);
    snprintf(rel, sizeof(rel), "%s/rel", root);
    snprintf(abs, sizeof(abs), "%s/abs", root);
    snprintf(pid_stat, sizeof(pid_stat), "/proc/%d/stat", (int)getpid());
    char pid_exe[64];
    snprintf(pid_exe, sizeof(pid_exe), "/proc/%d/exe", (int)getpid());
    char sub_link_in[64];
    char sub_link_out[64];
    char created_in[64];
    snprintf(sub_link_in, sizeof(sub_link_in), "%s/sub/link-in", root);
    snprintf(sub_link_out, sizeof(sub_link_out), "%s/sub/link-out", root);
    snprintf(created_in, sizeof(created_in), "%s/dangling-in", root);

    unlink(rel);
    unlink(abs);
    unlink(sub_link_in);
    unlink(sub_link_out);
    unlink(created_in);
    rmdir(sub);
    rmdir(root);

    CHECK(mkdir(root, 0700) == 0, "create fixture root");
    CHECK(mkdir(sub, 0700) == 0, "create fixture subdirectory");
    CHECK(symlink("sub", rel) == 0, "create relative symlink fixture");
    CHECK(symlink("/tmp", abs) == 0, "create absolute symlink fixture");

    int rootfd = open(root, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    int subfd = open(sub, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    int fsrootfd = open("/", O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    CHECK(rootfd >= 0 && subfd >= 0 && fsrootfd >= 0,
          "open fixture directories");
    if (rootfd < 0 || subfd < 0 || fsrootfd < 0) {
        return 1;
    }

    /* RESOLVE_BENEATH: relative opens work, everything above fails. */
    expect_open("BENEATH creates a relative file", rootfd, "beneath.txt",
                RESOLVE_BENEATH, O_CREAT | O_RDWR | O_CLOEXEC);
    expect_errno("BENEATH rejects an absolute path", rootfd, "/tmp/o2c/sub",
                 RESOLVE_BENEATH, O_RDONLY | O_CLOEXEC, EXDEV);
    expect_errno("BENEATH rejects a parent escape", rootfd,
                 "../o2c-escape", RESOLVE_BENEATH,
                 O_CREAT | O_WRONLY | O_CLOEXEC, EXDEV);
    expect_errno("BENEATH rejects an absolute symlink target", rootfd, "abs",
                 RESOLVE_BENEATH, O_RDONLY | O_DIRECTORY | O_CLOEXEC, EXDEV);
    expect_open("BENEATH follows an in-base relative symlink", rootfd, "rel",
                RESOLVE_BENEATH, O_RDONLY | O_DIRECTORY | O_CLOEXEC);

    /* A dangling symlink whose target stays inside the base must be created
     * through under BENEATH|O_CREAT (`sub/link-in -> ../dangling-in`
     * resolves to base/dangling-in); one escaping the base stays EXDEV. */
    CHECK(symlink("../dangling-in", sub_link_in) == 0,
          "create in-base dangling link fixture");
    expect_open("BENEATH creates through an in-base dangling symlink", rootfd,
                "sub/link-in", RESOLVE_BENEATH,
                O_CREAT | O_WRONLY | O_CLOEXEC);
    CHECK(symlink("../../dangling-out", sub_link_out) == 0,
          "create escaping dangling link fixture");
    expect_errno("BENEATH rejects an escaping dangling symlink", rootfd,
                 "sub/link-out", RESOLVE_BENEATH,
                 O_CREAT | O_WRONLY | O_CLOEXEC, EXDEV);

    /* RESOLVE_IN_ROOT: the dirfd acts as a chroot root. */
    expect_open("IN_ROOT resolves an absolute path inside the root", rootfd,
                "/sub", RESOLVE_IN_ROOT, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    expect_errno("IN_ROOT contains absolute paths", rootfd, "/etc",
                 RESOLVE_IN_ROOT, O_RDONLY | O_DIRECTORY | O_CLOEXEC, ENOENT);
    expect_open("IN_ROOT clamps .. at the root", rootfd, "sub/../..",
                RESOLVE_IN_ROOT, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    expect_errno("BENEATH and IN_ROOT are mutually exclusive", rootfd, "..",
                 RESOLVE_BENEATH | RESOLVE_IN_ROOT,
                 O_RDONLY | O_DIRECTORY | O_CLOEXEC, EINVAL);

    /* RESOLVE_NO_XDEV: mount boundary crossings fail with EXDEV. */
    expect_open("NO_XDEV allows same-mount resolution", rootfd, "rel",
                RESOLVE_NO_XDEV, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    expect_errno("NO_XDEV rejects crossing into procfs", fsrootfd, "proc",
                 RESOLVE_NO_XDEV, O_RDONLY | O_DIRECTORY | O_CLOEXEC, EXDEV);
    int procfd = open("/proc", O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    if (procfd >= 0) {
        expect_errno("NO_XDEV rejects climbing out of procfs", procfd, "..",
                     RESOLVE_NO_XDEV, O_RDONLY | O_DIRECTORY | O_CLOEXEC,
                     EXDEV);
        close(procfd);
    }

    /* RESOLVE_NO_MAGICLINKS: magic links fail, ordinary symlinks follow. */
    expect_errno("NO_MAGICLINKS rejects /proc/self/exe", fsrootfd,
                 "/proc/self/exe", RESOLVE_NO_MAGICLINKS,
                 O_RDONLY | O_CLOEXEC, ELOOP);
    expect_errno("NO_MAGICLINKS rejects /proc/self/fd/0", fsrootfd,
                 "/proc/self/fd/0", RESOLVE_NO_MAGICLINKS,
                 O_RDONLY | O_CLOEXEC, ELOOP);
    expect_errno("NO_MAGICLINKS rejects /proc/<pid>/ns/uts", fsrootfd,
                 "/proc/self/ns/uts", RESOLVE_NO_MAGICLINKS,
                 O_RDONLY | O_CLOEXEC, ELOOP);
    expect_errno("NO_SYMLINKS rejects /proc/<pid>/exe", fsrootfd, pid_exe,
                 RESOLVE_NO_SYMLINKS, O_RDONLY | O_CLOEXEC, ELOOP);
    expect_open("NO_MAGICLINKS opens a plain procfs file", fsrootfd,
                pid_stat, RESOLVE_NO_MAGICLINKS, O_RDONLY | O_CLOEXEC);
    expect_open("NO_MAGICLINKS still follows an ordinary symlink", rootfd,
                "rel", RESOLVE_NO_MAGICLINKS,
                O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    /* Existence is checked before the link restriction: missing proc
     * entries fail with ENOENT, not ELOOP (Linux link_path_walk order). */
    expect_errno("NO_MAGICLINKS ENOENT beats ELOOP for a missing exe",
                 fsrootfd, "/proc/999999/exe", RESOLVE_NO_MAGICLINKS,
                 O_RDONLY | O_CLOEXEC, ENOENT);
    expect_errno("NO_MAGICLINKS ENOENT beats ELOOP for a missing fd entry",
                 fsrootfd, "/proc/1/fd/999999", RESOLVE_NO_MAGICLINKS,
                 O_RDONLY | O_CLOEXEC, ENOENT);
    expect_errno("NO_SYMLINKS ENOENT beats ELOOP for an unknown ns entry",
                 fsrootfd, "/proc/1/ns/unknown", RESOLVE_NO_SYMLINKS,
                 O_RDONLY | O_CLOEXEC, ENOENT);

    /* RESOLVE_CACHED requires a dcache-only lookup this kernel cannot
     * provide; Linux fails such opens with EAGAIN (retry without the flag).
     * Creation with CACHED is rejected before any side effect. */
    expect_errno("CACHED with O_CREAT -> EAGAIN without side effects", rootfd,
                 "cached.txt", RESOLVE_CACHED,
                 O_CREAT | O_RDWR | O_CLOEXEC, EAGAIN);

    /* CACHED must not mask the fundamental errors Linux reports first:
     * EFAULT for a bad pathname, EBADF for an invalid dirfd, EINVAL for the
     * mutually exclusive scoping flags. */
    expect_errno("CACHED keeps EBADF for an invalid dirfd", -1, "rel",
                 RESOLVE_CACHED, O_RDONLY | O_CLOEXEC, EBADF);
    expect_errno("CACHED keeps EINVAL for exclusive scoping flags", rootfd,
                 "rel", RESOLVE_BENEATH | RESOLVE_IN_ROOT | RESOLVE_CACHED,
                 O_RDONLY | O_CLOEXEC, EINVAL);
    {
        const struct open_how cached_how = {
            .flags = O_RDONLY | O_CLOEXEC,
            .mode = 0,
            .resolve = RESOLVE_CACHED,
        };
        errno = 0;
        long bad = syscall(SYS_openat2, rootfd, (const char *)1, &cached_how,
                           sizeof(cached_how));
        CHECK(bad < 0 && errno == EFAULT,
              "CACHED keeps EFAULT for an invalid pathname");
    }

    /* Linux ignores dirfd for absolute pathnames; only RESOLVE_IN_ROOT keeps
     * using it (as the resolution root) and requires it to be valid. */
    expect_open("absolute path ignores an invalid dirfd", -1, pid_stat,
                RESOLVE_NO_MAGICLINKS, O_RDONLY | O_CLOEXEC);
    expect_errno("BENEATH absolute with an invalid dirfd -> EXDEV, not EBADF",
                 -1, "/proc/self/stat", RESOLVE_BENEATH,
                 O_RDONLY | O_CLOEXEC, EXDEV);
    expect_errno("an invalid dirfd still fails a relative path", -1, "tmp",
                 RESOLVE_NO_MAGICLINKS, O_RDONLY | O_DIRECTORY | O_CLOEXEC,
                 EBADF);
    expect_errno("IN_ROOT keeps requiring a valid dirfd", -1,
                 "/proc/self/stat", RESOLVE_IN_ROOT, O_RDONLY | O_CLOEXEC,
                 EBADF);

    /* Spatial constraints observe the whole walk, so magic-link paths are
     * resolved as ordinary paths instead of jumping to the backing object. */
    expect_errno("NO_XDEV rejects the procfs crossing on an exe path",
                 fsrootfd, "/proc/self/exe", RESOLVE_NO_XDEV,
                 O_RDONLY | O_CLOEXEC, EXDEV);
    expect_errno("IN_ROOT contains magic-link paths inside the root", rootfd,
                 "/proc/self/stat", RESOLVE_IN_ROOT,
                 O_RDONLY | O_CLOEXEC, ENOENT);

    /* A magic link actually reached under a spatial constraint must not be
     * re-resolved from its displayed target text (`pipe:[inode]`); Linux
     * refuses the scoped object jump with EXDEV (nd_jump_link IS_SCOPED and
     * NO_XDEV). Use a procfs dirfd so the walk reaches the entry without
     * crossing the mount first. */
    int scoped_procfd = open("/proc", O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    int pipe_fds[2] = {-1, -1};
    if (scoped_procfd >= 0 && pipe(pipe_fds) == 0) {
        char fd_link[64];
        snprintf(fd_link, sizeof(fd_link), "self/fd/%d", pipe_fds[0]);
        expect_errno("BENEATH rejects a reached proc fd magic link",
                     scoped_procfd, fd_link, RESOLVE_BENEATH,
                     O_RDONLY | O_CLOEXEC, EXDEV);
        expect_errno("IN_ROOT rejects a reached proc fd magic link",
                     scoped_procfd, fd_link, RESOLVE_IN_ROOT,
                     O_RDONLY | O_CLOEXEC, EXDEV);
        expect_errno("NO_XDEV rejects a reached proc fd magic link",
                     scoped_procfd, fd_link, RESOLVE_NO_XDEV,
                     O_RDONLY | O_CLOEXEC, EXDEV);
        expect_errno("IN_ROOT rejects a reached exe magic link",
                     scoped_procfd, "self/exe", RESOLVE_IN_ROOT,
                     O_RDONLY | O_CLOEXEC, EXDEV);
    } else {
        CHECK(0, "prepare a procfs dirfd and pipe for scoped magic-link cases");
    }
    if (pipe_fds[0] >= 0) {
        close(pipe_fds[0]);
    }
    if (pipe_fds[1] >= 0) {
        close(pipe_fds[1]);
    }
    if (scoped_procfd >= 0) {
        close(scoped_procfd);
    }

    /* O_PATH|O_NOFOLLOW on a final magic link yields a handle to the link
     * itself, even under NO_MAGICLINKS/NO_SYMLINKS (man 2 openat2). */
    expect_open("O_PATH|O_NOFOLLOW opens the final magic link itself",
                fsrootfd, pid_exe,
                RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS,
                O_PATH | O_NOFOLLOW | O_CLOEXEC);

    /* RESOLVE_NO_SYMLINKS alone (without the caller's O_NOFOLLOW) rejects the
     * final link with ELOOP; only an explicit O_PATH|O_NOFOLLOW returns the
     * link handle, so the constraint must not be folded into O_NOFOLLOW. */
    expect_errno("O_PATH without O_NOFOLLOW under NO_SYMLINKS -> ELOOP",
                 fsrootfd, pid_exe, RESOLVE_NO_SYMLINKS, O_PATH | O_CLOEXEC,
                 ELOOP);
    expect_errno("NO_SYMLINKS rejects an ordinary symlink under O_PATH",
                 rootfd, "rel", RESOLVE_NO_SYMLINKS, O_PATH | O_CLOEXEC,
                 ELOOP);

    /* O_CREAT|O_EXCL implies O_NOFOLLOW, so an existing symlink reports
     * EEXIST from the existence check rather than ELOOP. */
    expect_errno("O_CREAT|O_EXCL on an existing symlink -> EEXIST", rootfd,
                 "rel", RESOLVE_NO_SYMLINKS,
                 O_CREAT | O_EXCL | O_WRONLY | O_CLOEXEC, EEXIST);

    /* Restrictions combine. */
    uint64_t all = RESOLVE_BENEATH | RESOLVE_NO_XDEV | RESOLVE_NO_SYMLINKS;
    expect_open("BENEATH|NO_XDEV|NO_SYMLINKS creates a relative file",
                rootfd, "combo.txt",
                all, O_CREAT | O_RDWR | O_CLOEXEC);
    expect_errno("BENEATH|NO_XDEV|NO_SYMLINKS rejects an absolute path",
                 rootfd, "/tmp", all, O_RDONLY | O_DIRECTORY | O_CLOEXEC,
                 EXDEV);
    expect_errno("BENEATH|NO_XDEV|NO_SYMLINKS rejects a symlink", rootfd,
                 "rel", all, O_RDONLY | O_DIRECTORY | O_CLOEXEC, ELOOP);

    close(rootfd);
    close(subfd);
    close(fsrootfd);
    unlink(rel);
    unlink(abs);
    unlink("/tmp/o2c/beneath.txt");
    unlink("/tmp/o2c/combo.txt");
    rmdir(sub);
    rmdir(root);

    if (failures != 0) {
        printf("O2C_CONSTRAINTS_FAILED: %d failure(s)\n", failures);
        return 1;
    }
    puts("O2C_CONSTRAINTS_PASSED");
    return 0;
}
