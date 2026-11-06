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
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/wait.h>
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
    char link_file[64];
    char rel[64];
    char abs[64];
    char pid_stat[64];
    struct stat st;

    snprintf(sub, sizeof(sub), "%s/sub", root);
    snprintf(rel, sizeof(rel), "%s/rel", root);
    snprintf(abs, sizeof(abs), "%s/abs", root);
    snprintf(pid_stat, sizeof(pid_stat), "/proc/%d/stat", (int)getpid());
    char pid_exe[64];
    snprintf(pid_exe, sizeof(pid_exe), "/proc/%d/exe", (int)getpid());
    char sub_link_in[64];
    char sub_link_out[64];
    char created_in[64];
    char sub_link_slash[64];
    char sub_link_bare[64];
    char created_slash[64];
    char created_bare[64];
    snprintf(sub_link_in, sizeof(sub_link_in), "%s/sub/link-in", root);
    snprintf(sub_link_out, sizeof(sub_link_out), "%s/sub/link-out", root);
    snprintf(created_in, sizeof(created_in), "%s/dangling-in", root);
    snprintf(sub_link_slash, sizeof(sub_link_slash), "%s/sub/link-slash", root);
    snprintf(sub_link_bare, sizeof(sub_link_bare), "%s/sub/link-bare", root);
    snprintf(created_slash, sizeof(created_slash), "%s/dangling-slash", root);
    snprintf(created_bare, sizeof(created_bare), "%s/dangling-bare", root);
    char beneath_txt[64];
    char combo_txt[64];
    snprintf(link_file, sizeof(link_file), "%s/link-file", root);
    snprintf(beneath_txt, sizeof(beneath_txt), "%s/beneath.txt", root);
    snprintf(combo_txt, sizeof(combo_txt), "%s/combo.txt", root);
    /* Budget-chain fixtures: q-chain carries 40 expansions in the parent
     * walk (one past the shared limit once the leaf follow is added);
     * p-chain carries 39 so the leaf follow lands exactly on it. */
    char p_from[64];
    char p_to[64];
    char q_from[64];
    char q_to[64];
    char sub_leaf[64];

    /* Pre-run cleanup covers every name the case may create, so a previous
     * aborted run cannot turn leftover state into a bogus fixture failure. */
    const char *pre_clean[] = { rel,      abs,       link_file,   beneath_txt,
                                combo_txt, sub_link_in, sub_link_out, created_in,
                                sub_link_slash, sub_link_bare,
                                created_slash, created_bare,
                                "/tmp/o2c/cached-create.txt",
                                "/tmp/o2c/mnt-file" };
    (void)umount2("/tmp/o2c/mnt-file", MNT_DETACH);
    (void)rmdir("/tmp/o2c/ro-sub");
    for (size_t i = 0; i < sizeof(pre_clean) / sizeof(pre_clean[0]); i++) {
        unlink(pre_clean[i]);
    }
    for (int i = 1; i <= 40; i++) {
        snprintf(p_from, sizeof(p_from), "%s/p%02d", root, i);
        snprintf(q_from, sizeof(q_from), "%s/q%02d", root, i);
        unlink(p_from);
        unlink(q_from);
    }
    snprintf(sub_leaf, sizeof(sub_leaf), "%s/sub/leaf", root);
    unlink(sub_leaf);
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
    /* A symlink to a plain file resolves to that file: the final component
     * of a symlink target is not required to be a directory. */
    snprintf(link_file, sizeof(link_file), "%s/link-file", root);
    CHECK(symlink("beneath.txt", link_file) == 0,
          "create plain-file symlink fixture");
    expect_open("BENEATH follows a symlink to a plain file", rootfd,
                "link-file", RESOLVE_BENEATH, O_RDONLY | O_CLOEXEC);
    expect_errno("path below a plain-file symlink is ENOTDIR", rootfd,
                 "link-file/x", RESOLVE_BENEATH, O_RDONLY | O_CLOEXEC,
                 ENOTDIR);
    expect_errno("BENEATH rejects an absolute path", rootfd, "/tmp/o2c/sub",
                 RESOLVE_BENEATH, O_RDONLY | O_CLOEXEC, EXDEV);
    expect_errno("BENEATH rejects a parent escape", rootfd,
                 "../o2c-escape", RESOLVE_BENEATH,
                 O_CREAT | O_WRONLY | O_CLOEXEC, EXDEV);
    expect_errno("BENEATH rejects an absolute symlink target", rootfd, "abs",
                 RESOLVE_BENEATH, O_RDONLY | O_DIRECTORY | O_CLOEXEC, EXDEV);
    expect_open("BENEATH follows an in-base relative symlink", rootfd, "rel",
                RESOLVE_BENEATH, O_RDONLY | O_DIRECTORY | O_CLOEXEC);

    /* A trailing slash sets LOOKUP_FOLLOW|LOOKUP_DIRECTORY (Linux
     * open_last_lookups), so the final link is dereferenced even under
     * O_NOFOLLOW: a link to a directory opens, a link to a file is ENOTDIR,
     * and NO_SYMLINKS still rejects the follow with ELOOP. */
    expect_open("BENEATH trailing slash follows a final link to a directory",
                rootfd, "rel/", RESOLVE_BENEATH,
                O_RDONLY | O_NOFOLLOW | O_CLOEXEC);
    expect_open("BENEATH trailing slash overrides O_NOFOLLOW|O_DIRECTORY",
                rootfd, "rel/", RESOLVE_BENEATH,
                O_RDONLY | O_NOFOLLOW | O_DIRECTORY | O_CLOEXEC);
    expect_errno("a plain-file link with a trailing slash is ENOTDIR", rootfd,
                 "link-file/", RESOLVE_BENEATH,
                 O_RDONLY | O_NOFOLLOW | O_CLOEXEC, ENOTDIR);
    expect_errno("trailing slash does not bypass NO_SYMLINKS", rootfd, "rel/",
                 RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS,
                 O_RDONLY | O_NOFOLLOW | O_CLOEXEC, ELOOP);

    /* NO_XDEV covers the final component too: a file bind mount is a mount
     * boundary, and Linux traverses it inside `step_into` before `do_last`'s
     * O_EXCL check, so the combination is EXDEV rather than EEXIST. A
     * same-mount existing entry keeps EEXIST. */
    {
        char mnt_file[64];
        snprintf(mnt_file, sizeof(mnt_file), "%s/mnt-file", root);
        unlink(mnt_file);
        int target = open(mnt_file, O_CREAT | O_WRONLY | O_CLOEXEC, 0600);
        if (target >= 0) {
            close(target);
        }
        if (mount("/tmp/o2c/beneath.txt", mnt_file, NULL, MS_BIND, NULL) == 0) {
            expect_errno("NO_XDEV|O_CREAT|O_EXCL on a file bind mount -> EXDEV",
                         rootfd, "mnt-file", RESOLVE_NO_XDEV,
                         O_CREAT | O_EXCL | O_RDWR | O_CLOEXEC, EXDEV);
            expect_errno("O_CREAT|O_EXCL on a file bind mount without NO_XDEV",
                         rootfd, "mnt-file", 0,
                         O_CREAT | O_EXCL | O_RDWR | O_CLOEXEC, EEXIST);
            expect_errno("NO_XDEV|O_CREAT|O_EXCL on a same-mount entry -> EEXIST",
                         rootfd, "beneath.txt", RESOLVE_NO_XDEV,
                         O_CREAT | O_EXCL | O_RDWR | O_CLOEXEC, EEXIST);
            (void)umount2(mnt_file, MNT_DETACH);
        } else {
            CHECK(0, "bind mount a file for the NO_XDEV final-component case");
        }
        unlink(mnt_file);
    }

    /* A constrained O_TMPFILE open is not supported: the walker only opens
     * or creates named entries, so the request must fail with a clear error
     * before any side effect instead of touching the named object (Linux
     * `do_tmpfile` creates an anonymous inode inside `path_openat`). */
    expect_errno("NO_XDEV|O_TMPFILE on an existing directory -> EOPNOTSUPP",
                 rootfd, "sub", RESOLVE_NO_XDEV,
                 O_TMPFILE | O_RDWR | O_CLOEXEC, EOPNOTSUPP);
    expect_errno("BENEATH|O_TMPFILE on an existing file -> EOPNOTSUPP", rootfd,
                 "beneath.txt", RESOLVE_BENEATH,
                 O_TMPFILE | O_RDWR | O_CLOEXEC, EOPNOTSUPP);

    /* Creation authorization runs inside the create path: an entry that a
     * concurrent unlink removed between the existence probe and the VFS
     * lookup must not be created without the parent's write permission. User
     * space cannot force that window, so pin the enforcement directly: a
     * non-writable directory rejects O_CREAT with EACCES on both the plain
     * and the constrained path and leaves no entry behind. */
    {
        char ro_sub[64];
        snprintf(ro_sub, sizeof(ro_sub), "%s/ro-sub", root);
        CHECK(mkdir(ro_sub, 0555) == 0, "create read-only fixture directory");
        int rofd = open(ro_sub, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
        CHECK(rofd >= 0, "open the read-only fixture directory");
        if (rofd >= 0) {
            pid_t creator = fork();
            if (creator == 0) {
                if (setgid(1000) != 0 || setuid(1000) != 0) {
                    _exit(3);
                }
                errno = 0;
                int created = do_openat2(rofd, "new.txt", RESOLVE_BENEATH,
                                         O_CREAT | O_WRONLY | O_CLOEXEC, 0600);
                if (created >= 0 || errno != EACCES) {
                    _exit(4);
                }
                errno = 0;
                int plain =
                    openat(rofd, "new2.txt", O_CREAT | O_WRONLY | O_CLOEXEC, 0600);
                if (plain >= 0 || errno != EACCES) {
                    _exit(5);
                }
                _exit(0);
            }
            int status = 0;
            int reaped = creator > 0 && waitpid(creator, &status, 0) == creator;
            if (reaped && WIFEXITED(status) && WEXITSTATUS(status) == 0) {
                CHECK(1, "a non-writable directory rejects O_CREAT with EACCES");
            } else {
                printf("FAIL: a non-writable directory rejects O_CREAT with "
                       "EACCES (exit=%d)\n",
                       reaped && WIFEXITED(status) ? WEXITSTATUS(status) : -1);
                failures++;
            }
            close(rofd);
        }
        char created_path[80];
        snprintf(created_path, sizeof(created_path), "%s/ro-sub/new.txt", root);
        errno = 0;
        if (stat(created_path, &st) == 0 || errno != ENOENT) {
            CHECK(0, "a denied create leaves no file behind");
        } else {
            CHECK(1, "a denied create leaves no file behind");
        }
        snprintf(created_path, sizeof(created_path), "%s/ro-sub/new2.txt", root);
        errno = 0;
        if (stat(created_path, &st) == 0 || errno != ENOENT) {
            CHECK(0, "a denied plain create leaves no file behind");
        } else {
            CHECK(1, "a denied plain create leaves no file behind");
        }
        CHECK(rmdir(ro_sub) == 0, "remove the read-only fixture directory");
    }

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

    /* A dangling link whose target carries a trailing slash demands a
     * directory: O_CREAT must fail EISDIR and leave no created file behind
     * (Linux lookup_fast_for_open), on either side of the slash. */
    snprintf(sub_link_slash, sizeof(sub_link_slash), "%s/sub/link-slash", root);
    snprintf(sub_link_bare, sizeof(sub_link_bare), "%s/sub/link-bare", root);
    snprintf(created_slash, sizeof(created_slash), "%s/dangling-slash", root);
    snprintf(created_bare, sizeof(created_bare), "%s/dangling-bare", root);
    CHECK(symlink("../dangling-slash/", sub_link_slash) == 0,
          "create slash-target dangling link fixture");
    expect_errno("slash link target keeps its directory demand", rootfd,
                 "sub/link-slash", RESOLVE_BENEATH,
                 O_CREAT | O_WRONLY | O_CLOEXEC, EISDIR);
    CHECK(symlink("../dangling-bare", sub_link_bare) == 0,
          "create bare-target dangling link fixture");
    expect_errno("caller trailing slash keeps its directory demand", rootfd,
                 "sub/link-bare/", RESOLVE_BENEATH,
                 O_CREAT | O_WRONLY | O_CLOEXEC, EISDIR);
    errno = 0;
    if (stat(created_slash, &st) == 0 || errno != ENOENT) {
        CHECK(0, "slash link target creation leaves no file");
    } else {
        CHECK(1, "slash link target creation leaves no file");
    }
    errno = 0;
    if (stat(created_bare, &st) == 0 || errno != ENOENT) {
        CHECK(0, "caller trailing slash creation leaves no file");
    } else {
        CHECK(1, "caller trailing slash creation leaves no file");
    }

    /* The dot-only spelling names an existing entry: O_CREAT|O_EXCL reports
     * EEXIST on both the plain openat path and the constrained openat2 path
     * (Linux do_last). */
    errno = 0;
    int dot_excl = open(".", O_CREAT | O_EXCL | O_WRONLY | O_CLOEXEC, 0600);
    if (dot_excl >= 0) {
        close(dot_excl);
        errno = 0;
        CHECK(0, "plain openat O_CREAT|O_EXCL on . reports EEXIST");
    } else if (errno == EEXIST) {
        CHECK(1, "plain openat O_CREAT|O_EXCL on . reports EEXIST");
    } else {
        CHECK(0, "plain openat O_CREAT|O_EXCL on . reports EEXIST");
    }
    expect_errno("openat2 O_CREAT|O_EXCL on . reports EEXIST", rootfd, ".",
                 RESOLVE_BENEATH,
                 O_CREAT | O_EXCL | O_WRONLY | O_CLOEXEC, EEXIST);

    /* The symlink budget spans the parent walk and the final follow: a
     * 39-link parent chain plus the leaf link lands exactly on the limit
     * and opens; a 40-link parent chain pushes the leaf follow one past it
     * and fails ELOOP (Linux total_link_count covers the whole open). */
    for (int i = 1; i <= 40; i++) {
        snprintf(p_from, sizeof(p_from), "%s/p%02d", root, i);
        snprintf(q_from, sizeof(q_from), "%s/q%02d", root, i);
        if (i < 39) {
            snprintf(p_to, sizeof(p_to), "p%02d", i + 1);
        } else {
            snprintf(p_to, sizeof(p_to), "sub");
        }
        if (i < 40) {
            snprintf(q_to, sizeof(q_to), "q%02d", i + 1);
        } else {
            snprintf(q_to, sizeof(q_to), "sub");
        }
        CHECK(symlink(p_to, p_from) == 0, "create 39-link chain fixture");
        CHECK(symlink(q_to, q_from) == 0, "create 40-link chain fixture");
    }
    snprintf(sub_leaf, sizeof(sub_leaf), "%s/sub/leaf", root);
    CHECK(symlink("../beneath.txt", sub_leaf) == 0,
          "create leaf symlink fixture");
    expect_open("39 parent links plus the leaf stay within the budget",
                rootfd, "p01/leaf", RESOLVE_BENEATH, O_RDONLY | O_CLOEXEC);
    expect_errno("one past the shared symlink budget is ELOOP", rootfd,
                 "q01/leaf", RESOLVE_BENEATH, O_RDONLY | O_CLOEXEC, ELOOP);

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
     * provide, so every CACHED open fails EAGAIN here. On Linux only the
     * O_TRUNC|O_CREAT|O_TMPFILE combinations fail EAGAIN directly in
     * build_open_flags(); other cached hits succeed. Creation with CACHED
     * is rejected before any side effect. */
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

    /* Create/truncate/tmpfile requests are the one CACHED combination Linux
     * rejects straight from build_open_flags, before it copies the pathname
     * or resolves the dirfd. EAGAIN therefore wins over EFAULT/EBADF. */
    {
        const struct open_how cached_create = {
            .flags = O_CREAT | O_RDWR | O_CLOEXEC,
            .mode = 0600,
            .resolve = RESOLVE_CACHED,
        };
        errno = 0;
        long bad = syscall(SYS_openat2, rootfd, (const char *)1,
                           &cached_create, sizeof(cached_create));
        CHECK(bad < 0 && errno == EAGAIN,
              "CACHED|O_CREAT with an invalid pathname -> EAGAIN");
        errno = 0;
        long bad_fd = syscall(SYS_openat2, -1, "cached-create.txt",
                              &cached_create, sizeof(cached_create));
        CHECK(bad_fd < 0 && errno == EAGAIN,
              "CACHED|O_CREAT with an invalid dirfd -> EAGAIN");
        char cached_created[64];
        snprintf(cached_created, sizeof(cached_created), "%s/cached-create.txt",
                 root);
        errno = 0;
        if (stat(cached_created, &st) == 0 || errno != ENOENT) {
            CHECK(0, "CACHED|O_CREAT leaves no file behind");
        } else {
            CHECK(1, "CACHED|O_CREAT leaves no file behind");
        }
    }
    {
        const struct open_how cached_trunc = {
            .flags = O_TRUNC | O_RDWR | O_CLOEXEC,
            .mode = 0,
            .resolve = RESOLVE_CACHED,
        };
        errno = 0;
        long bad = syscall(SYS_openat2, rootfd, (const char *)1,
                           &cached_trunc, sizeof(cached_trunc));
        CHECK(bad < 0 && errno == EAGAIN,
              "CACHED|O_TRUNC with an invalid pathname -> EAGAIN");
    }
    expect_errno("CACHED|O_TMPFILE -> EAGAIN", rootfd, ".", RESOLVE_CACHED,
                 O_TMPFILE | O_RDWR | O_CLOEXEC, EAGAIN);

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
     * refuses the scoped object jump with EXDEV (`LOOKUP_IS_SCOPED`), and
     * for NO_XDEV refuses it when the target mount differs. Use a procfs
     * dirfd so the walk reaches the entry without crossing the mount first. */
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

        /* A descriptor naming a procfs object jumps to an object on the
         * same mount, so NO_XDEV allows it (Linux nd_jump_link only rejects
         * a different mount); BENEATH still refuses every scoped jump. */
        int proc_file_fd = open(pid_stat, O_RDONLY | O_CLOEXEC);
        if (proc_file_fd >= 0) {
            char proc_fd_link[64];
            snprintf(proc_fd_link, sizeof(proc_fd_link), "self/fd/%d",
                     proc_file_fd);
            expect_open("NO_XDEV follows a proc fd that stays on procfs",
                        scoped_procfd, proc_fd_link, RESOLVE_NO_XDEV,
                        O_RDONLY | O_CLOEXEC);
            expect_errno("BENEATH still rejects the same-mount proc fd jump",
                         scoped_procfd, proc_fd_link, RESOLVE_BENEATH,
                         O_RDONLY | O_CLOEXEC, EXDEV);
            close(proc_file_fd);
        } else {
            CHECK(0, "open a procfs file for the same-mount fd case");
        }
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
    unlink(link_file);
    unlink(sub_link_in);
    unlink(sub_link_out);
    unlink(sub_link_slash);
    unlink(sub_link_bare);
    unlink(created_in);
    unlink(created_slash);
    unlink(created_bare);
    unlink("/tmp/o2c/beneath.txt");
    unlink("/tmp/o2c/combo.txt");
    unlink("/tmp/o2c/cached-create.txt");
    /* The budget chains and their shared leaf are still linked inside the
     * fixture tree; remove them so both rmdir calls succeed. The pre-run
     * cleanup stays in place to recover leftovers from an aborted run. */
    for (int i = 1; i <= 40; i++) {
        snprintf(p_from, sizeof(p_from), "%s/p%02d", root, i);
        snprintf(q_from, sizeof(q_from), "%s/q%02d", root, i);
        unlink(p_from);
        unlink(q_from);
    }
    snprintf(sub_leaf, sizeof(sub_leaf), "%s/sub/leaf", root);
    unlink(sub_leaf);
    CHECK(rmdir(sub) == 0, "remove fixture subdirectory");
    CHECK(rmdir(root) == 0, "remove fixture root");

    if (failures != 0) {
        printf("O2C_CONSTRAINTS_FAILED: %d failure(s)\n", failures);
        return 1;
    }
    puts("O2C_CONSTRAINTS_PASSED");
    return 0;
}
