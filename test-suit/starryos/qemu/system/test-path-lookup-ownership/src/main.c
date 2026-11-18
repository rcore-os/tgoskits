/* Real syscall regression against Linux v7.1, commit
 * 8cd9520d35a6c38db6567e97dd93b1f11f185dc6:
 * fs/namei.c: reserve_stack, path_openat, follow_dotdot, nd_jump_link;
 * fs/dcache.c: __d_move; fs/open.c: build_open_flags.
 */
#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <ftw.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

/* Linux UAPI bits: 64-bit libc may define O_LARGEFILE as zero. */
#if defined(__aarch64__)
#define KERNEL_O_LARGEFILE 0400000
#else
#define KERNEL_O_LARGEFILE 00100000
#endif

#ifndef SYS_openat2
#define SYS_openat2 437
#endif

#define RESOLVE_NO_XDEV       0x01
#define RESOLVE_NO_MAGICLINKS 0x02
#define RESOLVE_NO_SYMLINKS   0x04
#define RESOLVE_BENEATH       0x08
#define RESOLVE_IN_ROOT       0x10
#define RESOLVE_CACHED        0x20

struct open_how {
    uint64_t flags;
    uint64_t mode;
    uint64_t resolve;
};

static int failures;

#define CHECK(condition, ...) do {                                          \
    if (condition) {                                                        \
        printf("PASS: ");                                                   \
    } else {                                                               \
        failures++;                                                        \
        printf("FAIL: ");                                                   \
    }                                                                      \
    printf(__VA_ARGS__);                                                    \
    putchar('\n');                                                          \
} while (0)

/* Setup failures are not product assertions and must not cascade. */
#define REQUIRE(condition) do {                                            \
    if (!(condition)) {                                                    \
        printf("FIXTURE_FAILED: %s errno=%d (%s)\n", #condition, errno,       \
               strerror(errno));                                         \
        failures++;                                                        \
        goto out;                                                          \
    }                                                                      \
} while (0)

static int open2(int dirfd, const char *path, int flags, uint64_t resolve)
{
    const struct open_how how = {
        .flags = (uint64_t)flags,
        .mode = flags & O_CREAT ? 0600 : 0,
        .resolve = resolve,
    };
    return (int)syscall(SYS_openat2, dirfd, path, &how, sizeof(how));
}

static int open_at(int dirfd, const char *path, int flags)
{
    return (int)syscall(SYS_openat, dirfd, path, flags, 0600);
}

static void close_fd(int fd)
{
    if (fd >= 0)
        syscall(SYS_close, fd);
}

/* CACHED may transiently ask for a retry while the held fixture stays pinned. */
static int cached_open(int dirfd, const char *path, int flags, uint64_t resolve)
{
    for (int attempt = 0; attempt < 8; attempt++) {
        errno = 0;
        int fd = open2(dirfd, path, flags, RESOLVE_CACHED | resolve);
        if (fd >= 0 || errno != EAGAIN)
            return fd;
    }
    return -1;
}

static void expect_open(int fd, int expected_errno, const char *name)
{
    int error = errno;
    CHECK(expected_errno ? fd == -1 && error == expected_errno : fd >= 0,
          "%s: fd=%d errno=%d expected=%d", name, fd, error, expected_errno);
    close_fd(fd);
}

static void expect_absent(int dirfd, const char *path, const char *name)
{
    struct stat st;
    errno = 0;
    int ret = fstatat(dirfd, path, &st, AT_SYMLINK_NOFOLLOW);
    int error = errno;
    CHECK(ret == -1 && error == ENOENT, "%s: stat=%d errno=%d", name, ret, error);
    /* Remove an erroneously created file so later operations are independent. */
    if (ret == 0)
        unlinkat(dirfd, path, 0);
}

static void expect_same(int fd, int expected, const char *name)
{
    struct stat a, b;
    int ra = fd < 0 ? -1 : fstat(fd, &a);
    int rb = expected < 0 ? -1 : fstat(expected, &b);
    CHECK(ra == 0 && rb == 0 && a.st_dev == b.st_dev && a.st_ino == b.st_ino,
          "%s: fstat=%d/%d", name, ra, rb);
    if (ra == 0 && rb == 0)
        printf("IDENTITY: actual=%llu:%llu expected=%llu:%llu\n",
               (unsigned long long)a.st_dev, (unsigned long long)a.st_ino,
               (unsigned long long)b.st_dev, (unsigned long long)b.st_ino);
    close_fd(fd);
}

static void link_budget(int rootfd)
{
    char name[32], target[32];
    int realfd = -1, fd = -1;
    REQUIRE(mkdirat(rootfd, "real", 0700) == 0);
    realfd = open_at(rootfd, "real", O_RDONLY | O_DIRECTORY);
    REQUIRE(realfd >= 0);
    /* p1 expands 40 parent links, p2 expands 39; final adds one more. */
    for (int i = 40; i >= 1; i--) {
        snprintf(name, sizeof(name), "p%d", i);
        if (i == 40)
            snprintf(target, sizeof(target), "real");
        else
            snprintf(target, sizeof(target), "p%d", i + 1);
        REQUIRE(symlinkat(target, rootfd, name) == 0);
    }
    REQUIRE(symlinkat("missing", realfd, "final") == 0);
    const char *apis[] = {"open", "openat", "openat2(resolve=0)"};
    for (int api = 0; api < 3; api++) {
        for (int create = 0; create <= 1; create++) {
            if (!create) {
                fd = open_at(realfd, "missing", O_CREAT | O_EXCL | O_RDWR);
                REQUIRE(fd >= 0);
                close_fd(fd);
                fd = -1;
            }
            for (int over = 0; over <= 1; over++) {
                const char *path = over ? "p1/final" : "p2/final";
                int flags = create ? O_CREAT | O_RDWR : O_RDONLY;
                errno = 0;
                if (api == 0) {
#ifdef SYS_open
                    fd = (int)syscall(SYS_open, path, flags, 0600);
#else
                    /* Architectures without legacy open use AT_FDCWD. */
                    fd = open_at(AT_FDCWD, path, flags);
#endif
                } else if (api == 1)
                    fd = open_at(rootfd, path, flags);
                else
                    fd = open2(rootfd, path, flags, 0);
                int error = errno;
                CHECK(over ? fd == -1 && error == ELOOP : fd >= 0,
                      "link-budget %s %s total=%d fd=%d errno=%d expected=%s",
                      apis[api], create ? "dangling-create" : "existing",
                      over ? 41 : 40, fd, error, over ? "ELOOP" : "success");
                close_fd(fd);
                fd = -1;
                if (create) {
                    if (over)
                        expect_absent(realfd, "missing",
                                      "over-budget creation leaves no target");
                    else {
                        fd = open_at(realfd, "missing", O_RDONLY);
                        REQUIRE(fd >= 0);
                        close_fd(fd);
                        fd = -1;
                        REQUIRE(unlinkat(realfd, "missing", 0) == 0);
                    }
                }
            }
            if (!create)
                REQUIRE(unlinkat(realfd, "missing", 0) == 0);
        }
    }
out:
    close_fd(fd);
    close_fd(realfd);
}

static void rename_identity(int rootfd)
{
    int oldparent = -1, newparent = -1, moved = -1, child = -1;
    int source = -1, victim = -1, named = -1;
    char data[8] = {0};
    REQUIRE(mkdirat(rootfd, "old", 0700) == 0);
    REQUIRE(mkdirat(rootfd, "new", 0700) == 0);
    REQUIRE(mkdirat(rootfd, "old/moved", 0700) == 0);
    REQUIRE(mkdirat(rootfd, "old/moved/child", 0700) == 0);
    oldparent = open_at(rootfd, "old", O_RDONLY | O_DIRECTORY);
    newparent = open_at(rootfd, "new", O_RDONLY | O_DIRECTORY);
    moved = open_at(rootfd, "old/moved", O_RDONLY | O_DIRECTORY);
    child = open_at(rootfd, "old/moved/child", O_RDONLY | O_DIRECTORY);
    REQUIRE(oldparent >= 0 && newparent >= 0 && moved >= 0 && child >= 0);
    REQUIRE(syscall(SYS_renameat2, oldparent, "moved", newparent, "moved", 0) == 0);
    expect_same(open_at(moved, "..", O_RDONLY | O_DIRECTORY), newparent,
                "rename updates held directory fd parent");
    expect_same(open_at(child, "../..", O_RDONLY | O_DIRECTORY), newparent,
                "rename updates held descendant fd ancestor");
    expect_same(open_at(rootfd, "new/moved", O_RDONLY | O_DIRECTORY), moved,
                "rename preserves moved directory identity");

    source = open_at(rootfd, "source", O_CREAT | O_EXCL | O_RDWR);
    victim = open_at(rootfd, "victim", O_CREAT | O_EXCL | O_RDWR);
    REQUIRE(source >= 0 && victim >= 0);
    REQUIRE(write(source, "source", 6) == 6 && write(victim, "victim", 6) == 6);
    REQUIRE(syscall(SYS_renameat2, rootfd, "source", rootfd, "victim", 0) == 0);
    REQUIRE(pread(victim, data, 6, 0) == 6);
    CHECK(memcmp(data, "victim", 6) == 0,
          "rename replacement keeps old target fd content");
    named = open_at(rootfd, "victim", O_RDONLY);
    REQUIRE(named >= 0);
    REQUIRE(pread(named, data, 6, 0) == 6);
    CHECK(memcmp(data, "source", 6) == 0, "rename destination has source content");
    expect_same(open_at(rootfd, "victim", O_RDONLY), source,
                "rename destination has source inode");
out:
    close_fd(named);
    close_fd(victim);
    close_fd(source);
    close_fd(child);
    close_fd(moved);
    close_fd(newparent);
    close_fd(oldparent);
}

static void scoped_lookup(int rootfd)
{
    int file = -1, fd = -1, procfd = -1;
    char magic[64];
    struct stat st;
    REQUIRE(mkdirat(rootfd, "scope", 0700) == 0);
    file = open_at(rootfd, "scope/file", O_CREAT | O_EXCL | O_RDWR);
    REQUIRE(file >= 0);
    REQUIRE(symlinkat("scope/file", rootfd, "relative") == 0);
    REQUIRE(symlinkat("/scope/file", rootfd, "absolute") == 0);

    expect_open(open2(rootfd, "scope/file", O_RDONLY, RESOLVE_BENEATH), 0,
                "BENEATH normal relative path");
    expect_open(open2(rootfd, "../", O_RDONLY | O_DIRECTORY, RESOLVE_BENEATH),
                EXDEV, "BENEATH rejects parent escape");
    expect_open(open2(rootfd, "/scope/file", O_RDONLY, RESOLVE_BENEATH),
                EXDEV, "BENEATH rejects absolute path");
    expect_open(open2(rootfd, "absolute", O_RDONLY, RESOLVE_BENEATH),
                EXDEV, "BENEATH rejects absolute symlink");
    expect_same(open2(rootfd, "/scope/file", O_RDONLY, RESOLVE_IN_ROOT), file,
                "IN_ROOT absolute path uses dirfd root");
    expect_same(open2(rootfd, "../../scope/file", O_RDONLY, RESOLVE_IN_ROOT),
                file, "IN_ROOT clamps parent traversal");
    expect_same(open2(rootfd, "absolute", O_RDONLY, RESOLVE_IN_ROOT), file,
                "IN_ROOT absolute symlink uses dirfd root");
    expect_open(open2(rootfd, "scope/file", O_RDONLY, RESOLVE_NO_SYMLINKS), 0,
                "NO_SYMLINKS permits regular path");
    expect_open(open2(rootfd, "relative", O_RDONLY, RESOLVE_NO_SYMLINKS),
                ELOOP, "NO_SYMLINKS rejects link traversal");
    fd = open2(rootfd, "relative", O_PATH | O_NOFOLLOW, RESOLVE_NO_SYMLINKS);
    CHECK(fd >= 0 && fstat(fd, &st) == 0 && S_ISLNK(st.st_mode),
          "NO_SYMLINKS O_PATH|O_NOFOLLOW holds final link fd=%d errno=%d",
          fd, errno);
    close_fd(fd);
    fd = -1;
    expect_same(open2(rootfd, "relative", O_RDONLY, RESOLVE_NO_MAGICLINKS), file,
                "NO_MAGICLINKS permits ordinary symlink");
    expect_open(open2(rootfd, "scope/file", O_RDONLY, RESOLVE_NO_MAGICLINKS), 0,
                "NO_MAGICLINKS permits regular path");
    procfd = open_at(AT_FDCWD, "/proc/self/fd", O_RDONLY | O_DIRECTORY);
    REQUIRE(procfd >= 0);
    snprintf(magic, sizeof(magic), "%d", file);
    expect_same(open2(procfd, magic, O_RDONLY, 0), file,
                "unrestricted proc fd magic link reaches held file");
    expect_open(open2(procfd, magic, O_RDONLY, RESOLVE_NO_MAGICLINKS), ELOOP,
                "NO_MAGICLINKS rejects proc fd jump");
    fd = open2(procfd, magic, O_PATH | O_NOFOLLOW, RESOLVE_NO_MAGICLINKS);
    CHECK(fd >= 0 && fstat(fd, &st) == 0 && S_ISLNK(st.st_mode),
          "NO_MAGICLINKS O_PATH|O_NOFOLLOW holds final magic link fd=%d errno=%d",
          fd, errno);
    close_fd(fd);
    fd = -1;
    /* Linux lookup_fast_for_open rejects trailing-slash O_CREAT with EISDIR. */
    expect_open(open2(rootfd, "trailing/", O_CREAT | O_RDWR, 0), EISDIR,
                "trailing slash rejects regular-file creation");
    expect_absent(rootfd, "trailing", "trailing slash leaves no created file");
out:
    close_fd(fd);
    close_fd(procfd);
    close_fd(file);
}

static void cached_links(int rootfd, int file)
{
    int fd = -1;
    struct stat st;
    char name[40], target[40];
    REQUIRE(syscall(SYS_symlinkat, "file", rootfd, "cached-relative") == 0);
    REQUIRE(syscall(SYS_symlinkat, "/file", rootfd, "cached-absolute") == 0);
    REQUIRE(mkdirat(rootfd, "cached-dir", 0700) == 0);
    REQUIRE(syscall(SYS_symlinkat, "cached-dir/", rootfd,
                    "cached-directory") == 0);
    expect_same(cached_open(rootfd, "cached-relative", O_PATH, 0), file,
                "CACHED follows an in-memory relative link");
    expect_same(cached_open(rootfd, "cached-absolute", O_PATH, RESOLVE_IN_ROOT),
                file, "CACHED IN_ROOT applies to an absolute link target");
    expect_open(cached_open(rootfd, "cached-absolute", O_PATH, RESOLVE_BENEATH),
                EXDEV, "CACHED BENEATH rejects an absolute link target");
    expect_same(cached_open(rootfd, "cached-relative", O_PATH,
                            RESOLVE_NO_MAGICLINKS), file,
                "CACHED NO_MAGICLINKS permits a cached text link");
    expect_open(cached_open(rootfd, "cached-relative", O_PATH,
                            RESOLVE_NO_SYMLINKS), ELOOP,
                "CACHED NO_SYMLINKS rejects text-link traversal");
    fd = cached_open(rootfd, "cached-relative", O_PATH | O_NOFOLLOW,
                     RESOLVE_NO_SYMLINKS);
    CHECK(fd >= 0 && fstat(fd, &st) == 0 && S_ISLNK(st.st_mode),
          "CACHED NO_SYMLINKS nofollow retains the link itself fd=%d errno=%d",
          fd, errno);
    close_fd(fd);
    fd = -1;
    expect_open(cached_open(rootfd, "cached-relative/", O_PATH, 0), ENOTDIR,
                "CACHED link traversal preserves a trailing directory requirement");
    expect_open(cached_open(rootfd, "cached-relative", O_RDONLY | O_NOFOLLOW, 0),
                ELOOP, "CACHED ordinary nofollow rejects the final text link");
    fd = cached_open(rootfd, "cached-directory", O_PATH, 0);
    CHECK(fd >= 0 && fstat(fd, &st) == 0 && S_ISDIR(st.st_mode),
          "CACHED follows a directory link with a trailing slash fd=%d errno=%d",
          fd, errno);
    close_fd(fd);
    fd = -1;
    expect_open(cached_open(rootfd, "cached-directory",
                            O_PATH | O_NOFOLLOW | O_DIRECTORY, 0), ENOTDIR,
                "CACHED directory flag does not override final nofollow");
    fd = cached_open(rootfd, "cached-directory/", O_PATH | O_NOFOLLOW, 0);
    CHECK(fd >= 0 && fstat(fd, &st) == 0 && S_ISDIR(st.st_mode),
          "CACHED input trailing slash follows its directory link fd=%d errno=%d",
          fd, errno);
    close_fd(fd);
    fd = -1;
    expect_same(cached_open(rootfd, "../../file", O_PATH, RESOLVE_IN_ROOT),
                file, "CACHED IN_ROOT clamps parent traversal");
    /* follow_dotdot_rcu returns ECHILD here; CACHED refuses unlazy as EAGAIN. */
    expect_open(cached_open(rootfd, "../file", O_PATH, RESOLVE_BENEATH), EAGAIN,
                "CACHED BENEATH cannot unlazy a parent escape");
    expect_open(cached_open(rootfd, "..", O_PATH | O_DIRECTORY,
                            RESOLVE_NO_XDEV), EAGAIN,
                "CACHED NO_XDEV cannot unlazy upward mount crossing");

    for (int count = 1; count <= 41; count++) {
        snprintf(name, sizeof(name), "cached-chain-%d", count);
        if (count == 1)
            snprintf(target, sizeof(target), "file");
        else
            snprintf(target, sizeof(target), "cached-chain-%d", count - 1);
        REQUIRE(syscall(SYS_symlinkat, target, rootfd, name) == 0);
    }
    /* Prime every target and atime: pick_link cannot touch atime in CACHED. */
    expect_same(open_at(rootfd, "cached-chain-40", O_PATH), file,
                "ordinary lookup primes the complete cached link chain");
    expect_open(open_at(rootfd, "cached-chain-41", O_PATH), ELOOP,
                "ordinary lookup primes the front link of the excessive chain");
    expect_same(cached_open(rootfd, "cached-chain-40", O_PATH, 0), file,
                "CACHED permits the complete 40-link budget");
    expect_open(cached_open(rootfd, "cached-chain-41", O_PATH, 0), ELOOP,
                "CACHED rejects a path exceeding the shared link budget");
out:
    close_fd(fd);
}

static void cached_readonly(int rootfd, int original)
{
    const char payload[] = "cached-open-content";
    char bytes[sizeof(payload)] = {0};
    int first = -1, second = -1, shared = -1, directory = -1, path = -1;
    int original_status = (int)syscall(SYS_fcntl, original, F_GETFL, 0);
    CHECK(original_status == (O_RDWR | KERNEL_O_LARGEFILE),
          "ordinary open exposes status flags without creation or descriptor bits status=%d",
          original_status);
    CHECK(syscall(SYS_fcntl, original, F_GETFD, 0) == FD_CLOEXEC,
          "ordinary descriptor retains close-on-exec independently of F_GETFL");
    REQUIRE(syscall(SYS_pwrite64, original, payload, sizeof(payload) - 1, 0)
                == (ssize_t)sizeof(payload) - 1);
    path = open_at(rootfd, "file", O_PATH | O_RDWR | O_NONBLOCK | O_CLOEXEC
                    | O_CREAT | O_EXCL | O_TRUNC | O_NOCTTY);
    REQUIRE(path >= 0);
    int path_status = (int)syscall(SYS_fcntl, path, F_GETFL, 0);
    CHECK(path_status == O_PATH,
          "ordinary O_PATH reports only path status despite ignored open bits status=%d",
          path_status);
    CHECK(syscall(SYS_fcntl, path, F_GETFD, 0) == FD_CLOEXEC,
          "ordinary O_PATH descriptor retains close-on-exec separately");
    first = cached_open(rootfd, "file", O_RDONLY | O_NONBLOCK | O_CLOEXEC
                        | O_NOCTTY, 0);
    CHECK(first >= 0, "CACHED opens a pinned ordinary reader fd=%d errno=%d",
          first, errno);
    if (first < 0)
        goto out;
    int status = (int)syscall(SYS_fcntl, first, F_GETFL, 0);
    CHECK(status == (O_RDONLY | O_NONBLOCK | KERNEL_O_LARGEFILE),
          "CACHED reader reports exact readonly nonblocking status flags status=%d",
          status);
    int descriptor = (int)syscall(SYS_fcntl, first, F_GETFD, 0);
    CHECK(descriptor >= 0 && (descriptor & FD_CLOEXEC) != 0,
          "CACHED reader installs descriptor close-on-exec state");
    second = cached_open(rootfd, "file", O_RDONLY, 0);
    REQUIRE(second >= 0);
    shared = (int)syscall(SYS_dup, first);
    REQUIRE(shared >= 0);
    CHECK(syscall(SYS_read, first, bytes, 4) == 4
              && memcmp(bytes, payload, 4) == 0,
          "CACHED reader uses the original inode's buffered data");
    CHECK(syscall(SYS_read, shared, bytes, 3) == 3
              && memcmp(bytes, payload + 4, 3) == 0,
          "dup of a CACHED reader shares its open-description cursor");
    CHECK(syscall(SYS_read, second, bytes, 3) == 3
              && memcmp(bytes, payload, 3) == 0,
          "independent CACHED reader starts with an independent cursor");
    CHECK(syscall(SYS_pread64, first, bytes, 3, 10) == 3
              && memcmp(bytes, payload + 10, 3) == 0,
          "positioned CACHED read uses the original buffered data");
    CHECK(syscall(SYS_read, first, bytes, 2) == 2
              && memcmp(bytes, payload + 7, 2) == 0,
          "positioned CACHED read leaves the shared cursor unchanged");
    errno = 0;
    CHECK(syscall(SYS_write, first, payload, 1) == -1 && errno == EBADF,
          "CACHED readonly description rejects writes errno=%d", errno);
out:
    close_fd(path);
    close_fd(shared);
    close_fd(second);
    close_fd(first);
    directory = cached_open(rootfd, ".", O_RDONLY | O_DIRECTORY, 0);
    CHECK(directory >= 0, "CACHED opens a pinned directory reader fd=%d errno=%d",
          directory, errno);
    if (directory >= 0) {
        char entries[512];
        CHECK(syscall(SYS_getdents64, directory, entries, sizeof(entries)) > 0,
              "CACHED directory description enumerates its actual directory");
    }
    close_fd(directory);
}

static void cached_fd_admission(int rootfd, int held)
{
    struct rlimit saved, limited;
    int changed = 0, probe = -1, opened = -1;
    REQUIRE(syscall(SYS_prlimit64, 0, RLIMIT_NOFILE, NULL, &saved) == 0);
    limited = saved;
    limited.rlim_cur = 0;
    REQUIRE(syscall(SYS_prlimit64, 0, RLIMIT_NOFILE, &limited, NULL) == 0);
    changed = 1;
    expect_open(open2(rootfd, NULL, O_PATH, RESOLVE_CACHED), EFAULT,
                "CACHED imports the pathname before fd exhaustion");
    expect_open(open2(rootfd, "", O_PATH, RESOLVE_CACHED), ENOENT,
                "CACHED empty pathname precedes fd exhaustion");
    expect_open(open2(rootfd, NULL, O_CREAT | O_RDONLY, RESOLVE_CACHED),
                EAGAIN, "CACHED mutation flags precede pathname and fd quota");
    expect_open(open2(rootfd, "file", O_PATH, RESOLVE_CACHED), EMFILE,
                "CACHED positive path honors the fd-number quota");
    expect_open(open2(rootfd, "cold", O_PATH, RESOLVE_CACHED), EMFILE,
                "CACHED fd exhaustion precedes a cold lookup");
    expect_open(open2(-1, ".", O_PATH, RESOLVE_CACHED), EMFILE,
                "CACHED fd exhaustion precedes invalid dirfd");
    expect_open(open2(rootfd, "/file", O_PATH,
                      RESOLVE_CACHED | RESOLVE_BENEATH), EMFILE,
                "CACHED fd exhaustion precedes absolute BENEATH rejection");
    REQUIRE(syscall(SYS_prlimit64, 0, RLIMIT_NOFILE, &saved, NULL) == 0);
    changed = 0;

    probe = (int)syscall(SYS_dup, held);
    REQUIRE(probe >= 0);
    int expected = probe;
    close_fd(probe);
    probe = -1;
    expect_open(open2(rootfd, "cold", O_PATH, RESOLVE_CACHED), EAGAIN,
                "CACHED failed lookup releases its reserved fd");
    expect_open(open2(-1, ".", O_PATH, RESOLVE_CACHED), EBADF,
                "CACHED bad dirfd releases its reserved fd");
    opened = cached_open(rootfd, "file", O_PATH, 0);
    CHECK(opened == expected,
          "CACHED rollback preserves the lowest free fd opened=%d expected=%d",
          opened, expected);
out:
    if (changed)
        CHECK(syscall(SYS_prlimit64, 0, RLIMIT_NOFILE, &saved, NULL) == 0,
              "restore fd quota after a failed admission check errno=%d", errno);
    close_fd(opened);
    close_fd(probe);
}

static void mount_boundary(int rootfd)
{
    int mountfd = -1, file = -1, cached = -1, mounted = 0;
    struct stat original, retained;
    REQUIRE(mkdirat(rootfd, "mount", 0700) == 0);
    /* The suite gives each program a private mount namespace. */
    REQUIRE(syscall(SYS_mount, "none", "mount", "tmpfs", 0, NULL) == 0);
    mounted = 1;
    mountfd = open_at(rootfd, "mount", O_RDONLY | O_DIRECTORY);
    REQUIRE(mountfd >= 0);
    file = open_at(mountfd, "file", O_CREAT | O_EXCL | O_RDWR | O_TRUNC
                   | O_NOCTTY | O_CLOEXEC);
    REQUIRE(file >= 0);
    expect_open(open2(rootfd, "scope/file", O_RDONLY, RESOLVE_NO_XDEV), 0,
                "NO_XDEV permits same mount");
    expect_same(open2(rootfd, "mount/file", O_RDONLY, 0), file,
                "unrestricted lookup crosses private tmpfs mount");
    expect_open(open2(rootfd, "mount/file", O_RDONLY, RESOLVE_NO_XDEV), EXDEV,
                "NO_XDEV rejects downward mount crossing");
    expect_open(open2(mountfd, "..", O_RDONLY | O_DIRECTORY, RESOLVE_NO_XDEV),
                EXDEV, "NO_XDEV rejects upward mount crossing");

    expect_open(open2(mountfd, "cold", O_PATH, RESOLVE_CACHED), EAGAIN,
                "CACHED does not infer a negative from an unobserved name");
    expect_open(open_at(mountfd, "cold", O_PATH), ENOENT,
                "ordinary tmpfs lookup confirms the name is absent");
    /* Linux simple_lookup marks tmpfs negatives DCACHE_DONTCACHE. */
    expect_open(open2(mountfd, "cold", O_PATH, RESOLVE_CACHED), EAGAIN,
                "CACHED cannot reuse a discarded tmpfs negative");
    REQUIRE(fstat(file, &original) == 0);
    cached = cached_open(mountfd, "file", O_PATH | O_CLOEXEC, 0);
    CHECK(cached >= 0, "CACHED opens the pinned positive entry fd=%d errno=%d",
          cached, errno);
    if (cached < 0)
        goto out;
    cached_readonly(mountfd, file);
    cached_fd_admission(mountfd, cached);
    cached_links(mountfd, file);
    CHECK(fstat(cached, &retained) == 0 && retained.st_dev == original.st_dev
              && retained.st_ino == original.st_ino,
          "CACHED descriptor retains the actual inode");
    int descriptor_flags = (int)syscall(SYS_fcntl, cached, F_GETFD, 0);
    CHECK(descriptor_flags >= 0 && (descriptor_flags & FD_CLOEXEC) != 0,
          "CACHED installs descriptor close-on-exec state");
    int cached_status = (int)syscall(SYS_fcntl, cached, F_GETFL, 0);
    CHECK(cached_status == O_PATH,
          "CACHED O_PATH excludes descriptor bits from status flags status=%d",
          cached_status);
    REQUIRE(unlinkat(mountfd, "file", 0) == 0);
    CHECK(fstat(cached, &retained) == 0 && retained.st_nlink == 0,
          "CACHED descriptor observes live metadata after unlink");
    close_fd(file);
    file = -1;
    close_fd(mountfd);
    mountfd = -1;
    errno = 0;
    int unmounted = (int)syscall(SYS_umount2, "mount", 0);
    CHECK(unmounted == -1 && errno == EBUSY,
          "CACHED descriptor alone keeps the actual mount busy errno=%d", errno);
    if (unmounted == 0) {
        mounted = 0;
        goto out;
    }
    close_fd(cached);
    cached = -1;
    unmounted = (int)syscall(SYS_umount2, "mount", 0);
    CHECK(unmounted == 0, "closing CACHED descriptor releases mount errno=%d",
          errno);
    if (unmounted == 0)
        mounted = 0;
out:
    close_fd(cached);
    close_fd(file);
    if (mountfd >= 0)
        unlinkat(mountfd, "file", 0);
    close_fd(mountfd);
    if (mounted)
        CHECK(syscall(SYS_umount2, "mount", 0) == 0,
              "unmount private tmpfs errno=%d", errno);
}

static int remove_entry(const char *path, const struct stat *st, int type,
                        struct FTW *walk)
{
    (void)st;
    (void)type;
    (void)walk;
    return remove(path);
}

int main(void)
{
    char fixture[] = "/tmp/path-lookup-ownership-XXXXXX";
    int saved_cwd = -1, rootfd = -1;
    int created = 0;
    setvbuf(stdout, NULL, _IONBF, 0);
    puts("PATH_LOOKUP_OWNERSHIP_BEGIN");
    saved_cwd = open_at(AT_FDCWD, ".", O_RDONLY | O_DIRECTORY);
    REQUIRE(saved_cwd >= 0);
    REQUIRE(mkdtemp(fixture) != NULL);
    created = 1;
    rootfd = open_at(AT_FDCWD, fixture, O_RDONLY | O_DIRECTORY);
    REQUIRE(rootfd >= 0);
    REQUIRE(fchdir(rootfd) == 0);
    link_budget(rootfd);
    rename_identity(rootfd);
    scoped_lookup(rootfd);
    mount_boundary(rootfd);
out:
    if (saved_cwd >= 0)
        CHECK(fchdir(saved_cwd) == 0, "restore cwd errno=%d", errno);
    close_fd(rootfd);
    close_fd(saved_cwd);
    if (created)
        CHECK(nftw(fixture, remove_entry, 16, FTW_DEPTH | FTW_PHYS | FTW_MOUNT) == 0,
              "remove isolated fixture errno=%d", errno);
    if (failures) {
        printf("PATH_LOOKUP_OWNERSHIP_FAILED: %d failure(s)\n", failures);
        return 1;
    }
    puts("PATH_LOOKUP_OWNERSHIP_PASSED");
    return 0;
}
