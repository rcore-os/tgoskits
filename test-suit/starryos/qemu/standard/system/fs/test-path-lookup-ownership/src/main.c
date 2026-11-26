/* Real syscall regression against Linux v7.1, commit
 * 8cd9520d35a6c38db6567e97dd93b1f11f185dc6:
 * fs/namei.c: reserve_stack, path_openat, follow_dotdot, nd_jump_link;
 * fs/dcache.c: __d_move; fs/open.c: build_open_flags.
 */
#define _GNU_SOURCE

#include <sys/statfs.h>
#include <errno.h>
#include <fcntl.h>
#include <ftw.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/mount.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <time.h>
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

static int same_link_metadata(const struct stat *a, const struct stat *b)
{
    return a->st_dev == b->st_dev && a->st_ino == b->st_ino
        && a->st_mode == b->st_mode && a->st_uid == b->st_uid
        && a->st_gid == b->st_gid && a->st_nlink == b->st_nlink
        && a->st_size == b->st_size
        && a->st_mtim.tv_sec == b->st_mtim.tv_sec
        && a->st_mtim.tv_nsec == b->st_mtim.tv_nsec
        && a->st_ctim.tv_sec == b->st_ctim.tv_sec
        && a->st_ctim.tv_nsec == b->st_ctim.tv_nsec;
}

static int time_between(struct timespec value, struct timespec first,
                        struct timespec last)
{
    return (value.tv_sec > first.tv_sec
            || (value.tv_sec == first.tv_sec && value.tv_nsec >= first.tv_nsec))
        && (value.tv_sec < last.tv_sec
            || (value.tv_sec == last.tv_sec && value.tv_nsec <= last.tv_nsec));
}

static void ordinary_link_times(int rootfd, int file, int link)
{
    const char *operations[] = {
        "openat2 follows the link", "openat follows the link",
        "readlinkat reads the named link", "readlinkat reads the retained link",
#ifdef SYS_readlink
        "readlink reads the named link",
#endif
    };
    struct timespec first, last;
    struct timespec times[2] = {{0, 0}, {0, UTIME_OMIT}};
    struct stat before, after;
    char text[16];

    REQUIRE(syscall(SYS_mount, NULL, "mount", NULL,
                    MS_REMOUNT | MS_STRICTATIME, NULL) == 0);
    for (size_t operation = 0; operation < sizeof(operations) / sizeof(operations[0]); operation++) {
        REQUIRE(syscall(SYS_clock_gettime, CLOCK_REALTIME, &first) == 0);
        times[0] = first;
        times[0].tv_sec += 3600;
        REQUIRE(syscall(SYS_utimensat, rootfd, "cached-relative", times,
                        AT_SYMLINK_NOFOLLOW) == 0);
        REQUIRE(fstat(link, &before) == 0);
        /* Linux current_time starts from the coarse realtime clock. */
        REQUIRE(syscall(SYS_clock_gettime, CLOCK_REALTIME_COARSE, &first) == 0);
        if (operation == 0) {
            expect_same(open2(rootfd, "cached-relative", O_PATH, 0), file,
                        operations[operation]);
        } else if (operation == 1) {
            expect_same(open_at(rootfd, "cached-relative", O_PATH), file,
                        operations[operation]);
        } else {
            ssize_t count;
            if (operation == 2)
                count = syscall(SYS_readlinkat, rootfd, "cached-relative", text, sizeof(text));
            else if (operation == 3)
                count = syscall(SYS_readlinkat, link, "", text, sizeof(text));
#ifdef SYS_readlink
            else
                count = syscall(SYS_readlink, "mount/cached-relative", text, sizeof(text));
#else
            else
                count = -1;
#endif
            CHECK(count == 4 && memcmp(text, "file", 4) == 0,
                  "%s returns the original text count=%zd errno=%d",
                  operations[operation], count, errno);
        }
        REQUIRE(syscall(SYS_clock_gettime, CLOCK_REALTIME, &last) == 0);
        REQUIRE(fstat(link, &after) == 0);
        CHECK(time_between(after.st_atim, first, last),
              "%s updates atime to the current clock", operations[operation]);
        CHECK(same_link_metadata(&before, &after),
              "%s preserves mtime, ctime and inode metadata", operations[operation]);
    }

    REQUIRE(syscall(SYS_utimensat, rootfd, "cached-relative", times,
                    AT_SYMLINK_NOFOLLOW) == 0);
    REQUIRE(fstat(link, &before) == 0);
    expect_same(open2(rootfd, "cached-relative", O_PATH | O_NOFOLLOW, 0), link,
                "ordinary final nofollow keeps the link without accessing its target");
    REQUIRE(fstat(link, &after) == 0);
    CHECK(after.st_atim.tv_sec == before.st_atim.tv_sec
              && after.st_atim.tv_nsec == before.st_atim.tv_nsec
              && same_link_metadata(&before, &after),
          "ordinary final nofollow leaves all link timestamps unchanged");
    REQUIRE(syscall(SYS_mount, NULL, "mount", NULL,
                    MS_REMOUNT | MS_NOATIME, NULL) == 0);
    expect_same(open2(rootfd, "cached-relative", O_PATH, 0), file,
                "ordinary noatime follows the target");
    CHECK(syscall(SYS_readlinkat, rootfd, "cached-relative", text, sizeof(text)) == 4
              && memcmp(text, "file", 4) == 0,
          "ordinary noatime displays the target");
    REQUIRE(fstat(link, &after) == 0);
    CHECK(after.st_atim.tv_sec == before.st_atim.tv_sec
              && after.st_atim.tv_nsec == before.st_atim.tv_nsec
              && same_link_metadata(&before, &after),
          "ordinary noatime leaves link timestamps unchanged");
out:
    return;
}

static void readonly_link_times(int rootfd)
{
    int source = -1, alias = -1, file = -1, link = -1;
    int mounted = 0, bound = 0, readonly = 0;
    struct timespec now, times[2] = {{0, 0}, {0, UTIME_OMIT}};
    struct stat before, after;
    char text[16];

    REQUIRE(mkdirat(rootfd, "time-source", 0700) == 0);
    REQUIRE(mkdirat(rootfd, "time-alias", 0700) == 0);
    REQUIRE(syscall(SYS_mount, "none", "mount/time-source", "tmpfs",
                    MS_STRICTATIME, NULL) == 0);
    mounted = 1;
    source = open_at(rootfd, "time-source", O_RDONLY | O_DIRECTORY);
    REQUIRE(source >= 0);
    /* No writable description or dirty file data is involved in this fixture. */
    REQUIRE(syscall(SYS_mknodat, source, "file", S_IFREG | 0600, 0) == 0);
    REQUIRE(syscall(SYS_symlinkat, "file", source, "link") == 0);
    file = open_at(source, "file", O_PATH);
    link = open_at(source, "link", O_PATH | O_NOFOLLOW);
    REQUIRE(file >= 0 && link >= 0);
    REQUIRE(syscall(SYS_clock_gettime, CLOCK_REALTIME, &now) == 0);
    times[0] = now;
    times[0].tv_sec += 3600;
    REQUIRE(syscall(SYS_utimensat, source, "link", times, AT_SYMLINK_NOFOLLOW) == 0);
    REQUIRE(fstat(link, &before) == 0);

    REQUIRE(syscall(SYS_mount, "mount/time-source", "mount/time-alias", NULL,
                    MS_BIND, NULL) == 0);
    bound = 1;
    REQUIRE(syscall(SYS_mount, NULL, "mount/time-alias", NULL,
                    MS_REMOUNT | MS_BIND | MS_RDONLY | MS_STRICTATIME, NULL) == 0);
    alias = open_at(rootfd, "time-alias", O_RDONLY | O_DIRECTORY);
    REQUIRE(alias >= 0);
    expect_open(open2(alias, "link", O_PATH, RESOLVE_CACHED), EAGAIN,
                "mount-local readonly does not suppress cached link timestamp work");
    CHECK(syscall(SYS_readlinkat, alias, "link", text, sizeof(text)) == 4
              && memcmp(text, "file", 4) == 0,
          "mount-local readonly permits ordinary readlink despite timestamp failure");
    REQUIRE(fstat(link, &after) == 0);
    CHECK(after.st_atim.tv_sec == before.st_atim.tv_sec
              && after.st_atim.tv_nsec == before.st_atim.tv_nsec
              && same_link_metadata(&before, &after),
          "mount-local readonly leaves link timestamps unchanged");

    REQUIRE(syscall(SYS_mount, NULL, "mount/time-source", NULL,
                    MS_REMOUNT | MS_RDONLY | MS_STRICTATIME, NULL) == 0);
    readonly = 1;
    expect_same(cached_open(source, "link", O_PATH, 0), file,
                "shared filesystem readonly suppresses cached link timestamp work");
    CHECK(syscall(SYS_readlinkat, source, "link", text, sizeof(text)) == 4
              && memcmp(text, "file", 4) == 0,
          "shared filesystem readonly permits ordinary readlink");
    REQUIRE(fstat(link, &after) == 0);
    CHECK(after.st_atim.tv_sec == before.st_atim.tv_sec
              && after.st_atim.tv_nsec == before.st_atim.tv_nsec
              && same_link_metadata(&before, &after),
          "shared filesystem readonly leaves link timestamps unchanged");
out:
    if (readonly)
        CHECK(syscall(SYS_mount, NULL, "mount/time-source", NULL,
                      MS_REMOUNT | MS_STRICTATIME, NULL) == 0,
              "restore private source writability errno=%d", errno);
    close_fd(alias);
    close_fd(source);
    close_fd(file);
    close_fd(link);
    if (bound)
        CHECK(syscall(SYS_umount2, "mount/time-alias", 0) == 0,
              "unmount private timestamp alias errno=%d", errno);
    if (mounted)
        CHECK(syscall(SYS_umount2, "mount/time-source", 0) == 0,
              "unmount private timestamp source errno=%d", errno);
}

static void cached_link_times(int rootfd, int file)
{
    int link = -1, changed = 0;
    struct stat before, after;
    struct timespec now;
    struct timespec times[2] = {{0, 0}, {0, UTIME_OMIT}};

    link = open_at(rootfd, "cached-relative", O_PATH | O_NOFOLLOW);
    REQUIRE(link >= 0);
    REQUIRE(syscall(SYS_mount, NULL, "mount", NULL,
                    MS_REMOUNT | MS_RELATIME, NULL) == 0);
    changed = 1;
    REQUIRE(syscall(SYS_utimensat, rootfd, "cached-relative", times,
                    AT_SYMLINK_NOFOLLOW) == 0);
    REQUIRE(fstat(link, &before) == 0);
    expect_open(open2(rootfd, "cached-relative", O_PATH, RESOLVE_CACHED),
                EAGAIN, "CACHED refuses a link requiring an atime update");
    REQUIRE(fstat(link, &after) == 0);
    CHECK(after.st_atim.tv_sec == before.st_atim.tv_sec
              && after.st_atim.tv_nsec == before.st_atim.tv_nsec,
          "CACHED rejection leaves link atime unchanged");
    expect_same(cached_open(rootfd, "cached-relative", O_PATH | O_NOFOLLOW,
                            RESOLVE_NO_SYMLINKS), link,
                "CACHED final nofollow does not require a link atime update");
    expect_open(open2(rootfd, "cached-relative", O_PATH,
                      RESOLVE_CACHED | RESOLVE_NO_SYMLINKS), ELOOP,
                "CACHED link prohibition precedes an atime retry");

    REQUIRE(syscall(SYS_clock_gettime, CLOCK_REALTIME, &now) == 0);
    times[0] = now;
    times[0].tv_sec += 3600;
    REQUIRE(syscall(SYS_utimensat, rootfd, "cached-relative", times,
                    AT_SYMLINK_NOFOLLOW) == 0);
    REQUIRE(fstat(link, &before) == 0);
    REQUIRE(before.st_atim.tv_sec > before.st_mtim.tv_sec
                && before.st_atim.tv_sec > before.st_ctim.tv_sec);
    expect_same(cached_open(rootfd, "cached-relative", O_PATH, 0), file,
                "CACHED relatime follows a link with no timestamp work");
    REQUIRE(fstat(link, &after) == 0);
    CHECK(after.st_atim.tv_sec == before.st_atim.tv_sec
              && after.st_atim.tv_nsec == before.st_atim.tv_nsec
              && after.st_ctim.tv_sec == before.st_ctim.tv_sec
              && after.st_ctim.tv_nsec == before.st_ctim.tv_nsec,
          "CACHED successful link traversal leaves timestamps unchanged");
    expect_same(open2(rootfd, "cached-relative", O_PATH, 0), file,
                "ordinary relatime follows a link requiring no timestamp work");
    REQUIRE(fstat(link, &after) == 0);
    CHECK(after.st_atim.tv_sec == before.st_atim.tv_sec
              && after.st_atim.tv_nsec == before.st_atim.tv_nsec
              && same_link_metadata(&before, &after),
          "ordinary relatime leaves already current link metadata unchanged");

    REQUIRE(syscall(SYS_mount, NULL, "mount", NULL,
                    MS_REMOUNT | MS_STRICTATIME, NULL) == 0);
    expect_open(open2(rootfd, "cached-relative", O_PATH, RESOLVE_CACHED),
                EAGAIN, "CACHED strictatime requires an ordinary traversal");
    REQUIRE(syscall(SYS_mount, NULL, "mount", NULL, MS_REMOUNT, NULL) == 0);
    expect_open(open2(rootfd, "cached-relative", O_PATH, RESOLVE_CACHED),
                EAGAIN, "remount without an atime mode preserves strictatime");
    REQUIRE(syscall(SYS_mount, NULL, "mount", NULL,
                    MS_REMOUNT | MS_STRICTATIME | MS_NOATIME, NULL) == 0);
    expect_open(open2(rootfd, "cached-relative", O_PATH, RESOLVE_CACHED),
                EAGAIN, "explicit strictatime overrides a noatime request");
    ordinary_link_times(rootfd, file, link);
out:
    if (changed) {
        CHECK(syscall(SYS_mount, NULL, "mount", NULL,
                      MS_REMOUNT | MS_NOATIME, NULL) == 0,
              "restore the private noatime mount errno=%d", errno);
    }
    close_fd(link);
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
    /* The private mount is noatime; prime every name before cached traversal. */
    expect_same(open_at(rootfd, "cached-relative", O_PATH), file,
                "ordinary lookup primes the relative cached link");
    expect_same(open2(rootfd, "cached-absolute", O_PATH, RESOLVE_IN_ROOT), file,
                "ordinary IN_ROOT lookup primes the absolute cached link");
    expect_open(open_at(rootfd, "cached-directory", O_PATH | O_DIRECTORY), 0,
                "ordinary lookup primes the cached directory link");
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
    /* Prime every target on the private noatime mount. */
    expect_same(open_at(rootfd, "cached-chain-40", O_PATH), file,
                "ordinary lookup primes the complete cached link chain");
    expect_open(open_at(rootfd, "cached-chain-41", O_PATH), ELOOP,
                "ordinary lookup primes the front link of the excessive chain");
    expect_same(cached_open(rootfd, "cached-chain-40", O_PATH, 0), file,
                "CACHED permits the complete 40-link budget");
    expect_open(cached_open(rootfd, "cached-chain-41", O_PATH, 0), ELOOP,
                "CACHED rejects a path exceeding the shared link budget");
    cached_link_times(rootfd, file);
    readonly_link_times(rootfd);
out:
    close_fd(fd);
}

static void path_commands(int fd, const char *name)
{
    const int denied[] = {
        F_SETFL, F_SETOWN, F_GETOWN, F_GETPIPE_SZ, F_SETPIPE_SZ,
        F_GET_SEALS, F_ADD_SEALS, F_GETLK, F_SETLK, F_SETLKW,
        F_OFD_GETLK, F_OFD_SETLK, F_OFD_SETLKW,
        F_SETSIG, F_GETSIG, F_SETOWN_EX, F_GETOWN_EX,
        1035, 1036, 1037, 1038, /* Linux read/write hint commands. */
        0x7fffffff,
    };
    int duplicate = -1, cloexec = -1;
    int initial = (int)syscall(SYS_fcntl, fd, F_GETFL, 0);
    int descriptor = (int)syscall(SYS_fcntl, fd, F_GETFD, 0);
    REQUIRE(initial >= 0 && (initial & O_PATH) && descriptor >= 0);
    for (size_t i = 0; i < sizeof(denied) / sizeof(denied[0]); i++) {
        errno = 0;
        long result = syscall(SYS_fcntl, fd, denied[i], 0);
        int error = errno;
        CHECK(result == -1 && error == EBADF,
              "%s rejects restricted fcntl before arguments cmd=%d result=%ld errno=%d",
              name, denied[i], result, error);
    }
    CHECK(syscall(SYS_fcntl, fd, F_SETFD, descriptor ^ FD_CLOEXEC) == 0
              && syscall(SYS_fcntl, fd, F_GETFD, 0) == (descriptor ^ FD_CLOEXEC),
          "%s permits descriptor flag changes", name);
    duplicate = (int)syscall(SYS_fcntl, fd, F_DUPFD, 0);
    cloexec = (int)syscall(SYS_fcntl, fd, F_DUPFD_CLOEXEC, 0);
    CHECK(duplicate >= 0 && cloexec >= 0
              && syscall(SYS_fcntl, duplicate, F_GETFL, 0) == initial
              && syscall(SYS_fcntl, cloexec, F_GETFL, 0) == initial
              && syscall(SYS_fcntl, duplicate, F_GETFD, 0) == 0
              && syscall(SYS_fcntl, cloexec, F_GETFD, 0) == FD_CLOEXEC,
          "%s permits both fcntl duplication forms with independent fd flags", name);
out:
    if (descriptor >= 0)
        CHECK(syscall(SYS_fcntl, fd, F_SETFD, descriptor) == 0
                  && syscall(SYS_fcntl, fd, F_GETFL, 0) == initial,
              "%s restores fd flags and preserves path status", name);
    close_fd(cloexec);
    close_fd(duplicate);
}

static void description_status(int fd, int independent, const char *name)
{
    const int mutable = O_APPEND | O_NONBLOCK;
    const int states[] = {0, O_APPEND, O_NONBLOCK, O_APPEND | O_NONBLOCK, 0};
    int shared = -1;
    int initial = (int)syscall(SYS_fcntl, fd, F_GETFL, 0);
    int other_initial = (int)syscall(SYS_fcntl, independent, F_GETFL, 0);
    int descriptor = (int)syscall(SYS_fcntl, fd, F_GETFD, 0);
    REQUIRE(initial >= 0 && other_initial >= 0 && descriptor >= 0);
    shared = (int)syscall(SYS_dup, fd);
    REQUIRE(shared >= 0);
    for (size_t i = 0; i < sizeof(states) / sizeof(states[0]); i++) {
        int changed = (int)syscall(SYS_fcntl, shared, F_SETFL, states[i]);
        int actual = (int)syscall(SYS_fcntl, fd, F_GETFL, 0);
        int duplicate = (int)syscall(SYS_fcntl, shared, F_GETFL, 0);
        int other = (int)syscall(SYS_fcntl, independent, F_GETFL, 0);
        int expected = (initial & ~mutable) | states[i];
        CHECK(changed == 0 && actual == expected && duplicate == expected
                  && other == other_initial,
              "%s mutable status is shared only by dup state=%d set=%d status=%d/%d expected=%d independent=%d/%d",
              name, states[i], changed, actual, duplicate, expected,
              other, other_initial);
    }
    CHECK(syscall(SYS_fcntl, fd, F_GETFD, 0) == descriptor
              && syscall(SYS_fcntl, shared, F_GETFD, 0) == 0,
          "%s status changes preserve independent descriptor flags", name);
out:
    if (shared >= 0)
        CHECK(syscall(SYS_fcntl, shared, F_SETFL, initial & mutable) == 0,
              "%s restores its original status flags", name);
    close_fd(shared);
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
    path_commands(path, "ordinary O_PATH file");
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
    description_status(original, second, "ordinary file");
    description_status(first, second, "CACHED file");
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
    directory = cached_open(rootfd, ".", O_RDONLY | O_DIRECTORY | O_NONBLOCK, 0);
    CHECK(directory >= 0, "CACHED opens a pinned directory reader fd=%d errno=%d",
          directory, errno);
    if (directory >= 0) {
        char entries[512];
        CHECK(syscall(SYS_getdents64, directory, entries, sizeof(entries)) > 0,
              "CACHED directory description enumerates its actual directory");
        int native = open_at(rootfd, ".", O_RDONLY | O_DIRECTORY | O_NONBLOCK);
        CHECK(native >= 0, "ordinary directory status fixture fd=%d", native);
        if (native >= 0) {
            description_status(directory, native, "CACHED directory");
            description_status(native, directory, "ordinary directory");
        }
        close_fd(native);
        int native_path = open_at(rootfd, ".", O_PATH | O_DIRECTORY);
        int cached_path = cached_open(rootfd, ".", O_PATH | O_DIRECTORY, 0);
        CHECK(native_path >= 0 && cached_path >= 0,
              "ordinary/CACHED O_PATH directory fixtures fd=%d/%d",
              native_path, cached_path);
        if (native_path >= 0)
            path_commands(native_path, "ordinary O_PATH directory");
        if (cached_path >= 0)
            path_commands(cached_path, "CACHED O_PATH directory");
        close_fd(cached_path);
        close_fd(native_path);
    }
    close_fd(directory);
}

/* Complete a cached lookup through a real writable open description. */
static void cached_write_completion(int rootfd, int original)
{
    char saved[64], bytes[8];
    int first = -1, second = -1, shared = -1;
    int changed = 0;
    void *mapped = MAP_FAILED;
    size_t mapping_len = 0;
    ssize_t length = syscall(SYS_pread64, original, saved, sizeof(saved), 0);
    REQUIRE(length >= 8 && length < (ssize_t)sizeof(saved));
    first = cached_open(rootfd, "file", O_RDWR | O_NONBLOCK | O_CLOEXEC, 0);
    CHECK(first >= 0, "CACHED completes an ordinary writable open fd=%d errno=%d",
          first, errno);
    if (first < 0)
        goto out;
    int status = (int)syscall(SYS_fcntl, first, F_GETFL, 0);
    CHECK(status == (O_RDWR | O_NONBLOCK | KERNEL_O_LARGEFILE),
          "CACHED writer exposes its actual read/write status flags status=%d",
          status);
    second = cached_open(rootfd, "file", O_RDWR, 0);
    REQUIRE(second >= 0);
    description_status(first, second, "CACHED writable file");
    shared = (int)syscall(SYS_dup, first);
    REQUIRE(shared >= 0);
    CHECK(syscall(SYS_read, first, bytes, 2) == 2
              && memcmp(bytes, saved, 2) == 0,
          "CACHED writable description reads the original inode");
    changed = 1;
    CHECK(syscall(SYS_write, shared, "WX", 2) == 2,
          "dup of a CACHED writer writes at the shared cursor");
    CHECK(syscall(SYS_pread64, original, bytes, 2, 2) == 2
              && memcmp(bytes, "WX", 2) == 0,
          "ordinary reader observes the CACHED writer's actual bytes");
    CHECK(syscall(SYS_read, second, bytes, 4) == 4
              && memcmp(bytes, saved, 2) == 0
              && memcmp(bytes + 2, "WX", 2) == 0,
          "independent CACHED writer starts with its own cursor");
    CHECK(syscall(SYS_pwrite64, first, "Q", 1, 6) == 1
              && syscall(SYS_pread64, original, bytes, 1, 6) == 1
              && bytes[0] == 'Q',
          "positioned CACHED write reaches the retained inode");
    CHECK(syscall(SYS_read, shared, bytes, 2) == 2
              && memcmp(bytes, saved + 4, 2) == 0,
          "positioned CACHED write preserves the shared cursor");
    struct stat before_empty, after_empty;
    char before_bytes[sizeof(saved)], after_bytes[sizeof(saved)];
    REQUIRE(fstat(original, &before_empty) == 0 && before_empty.st_size == length);
    REQUIRE(syscall(SYS_pread64, original, before_bytes, length, (off_t)0) == length);
    off_t cursor = lseek(first, 0, SEEK_CUR);
    REQUIRE(cursor >= 0);
    off_t beyond = before_empty.st_size + 3;
    struct iovec empty = {.iov_base = bytes, .iov_len = 0};
    for (int append = 0; append < 2; append++) {
        REQUIRE(syscall(SYS_fcntl, first, F_SETFL,
                        (long)((status & O_NONBLOCK) | (append ? O_APPEND : 0))) == 0);
        errno = 0;
        ssize_t transferred = syscall(SYS_sendfile, first, original, NULL, (size_t)0);
        CHECK(append ? transferred == -1 && errno == EINVAL : transferred == 0,
              "CACHED file transfer observes current append state append=%d", append);
        for (int kind = 0; kind < 6; kind++) {
            int positioned = kind == 2 || kind == 3 || kind == 5;
            off_t expected_cursor = positioned ? cursor : beyond;
            REQUIRE(lseek(first, expected_cursor, SEEK_SET) == expected_cursor);
            ssize_t result;
            switch (kind) {
            case 0:
                result = syscall(SYS_write, first, bytes, (size_t)0);
                break;
            case 1:
                result = syscall(SYS_writev, first, &empty, (unsigned long)1);
                break;
            case 2:
                result = syscall(SYS_pwrite64, first, bytes, (size_t)0, beyond);
                break;
            case 3:
                result = syscall(SYS_pwritev, first, &empty, (unsigned long)1,
                                 beyond, 0UL);
                break;
            case 4:
                result = syscall(SYS_pwritev2, first, &empty, (unsigned long)1,
                                 (off_t)-1, 0UL, 0UL);
                break;
            default:
                result = syscall(SYS_pwritev2, first, &empty, (unsigned long)1,
                                 beyond, 0UL, 0UL);
                break;
            }
            REQUIRE(fstat(original, &after_empty) == 0);
            CHECK(result == 0 && after_empty.st_size == before_empty.st_size
                      && lseek(first, 0, SEEK_CUR) == expected_cursor,
                  "empty CACHED write preserves size and cursor kind=%d append=%d",
                      kind, append);
            /* Restore a failed candidate before the next independent observation. */
            if (after_empty.st_size != before_empty.st_size)
                REQUIRE(syscall(SYS_ftruncate, first, before_empty.st_size) == 0);
            REQUIRE(syscall(SYS_pread64, original, after_bytes, length, (off_t)0) == length);
            CHECK(memcmp(after_bytes, before_bytes, length) == 0,
                  "empty CACHED write preserves existing contents kind=%d append=%d",
                      kind, append);
        }
        if (append) {
            REQUIRE(lseek(first, cursor, SEEK_SET) == cursor);
            ssize_t written = syscall(SYS_write, first, "AP", (size_t)2);
            REQUIRE(fstat(original, &after_empty) == 0);
            CHECK(written == 2 && after_empty.st_size == before_empty.st_size + 2
                      && lseek(shared, 0, SEEK_CUR) == before_empty.st_size + 2
                      && syscall(SYS_pread64, original, after_bytes, (size_t)2,
                                 before_empty.st_size) == 2
                      && memcmp(after_bytes, "AP", 2) == 0,
                  "nonempty CACHED append uses EOF and advances the shared cursor");
            REQUIRE(syscall(SYS_ftruncate, first, before_empty.st_size) == 0);
        }
    }
    REQUIRE(syscall(SYS_fcntl, first, F_SETFL, (long)(status & O_NONBLOCK)) == 0);
    mapping_len = (size_t)length;
    mapped = (void *)syscall(SYS_mmap, NULL, mapping_len, PROT_READ,
                            MAP_PRIVATE, first, (off_t)0);
    REQUIRE(mapped != MAP_FAILED);
    REQUIRE(lseek(first, (off_t)0, SEEK_SET) == 0);
    CHECK(syscall(SYS_write, shared, mapped, mapping_len) == length,
          "CACHED writer reads its retained file mapping");
    REQUIRE(fstat(original, &after_empty) == 0);
    CHECK(after_empty.st_size == before_empty.st_size
              && lseek(first, (off_t)0, SEEK_CUR) == length
              && syscall(SYS_pread64, original, after_bytes, mapping_len,
                         (off_t)0) == length
              && memcmp(after_bytes, before_bytes, mapping_len) == 0,
          "same-file mapped source preserves contents and advances the shared cursor");
    REQUIRE(syscall(SYS_munmap, mapped, mapping_len) == 0);
    mapped = MAP_FAILED;
    REQUIRE(lseek(first, cursor, SEEK_SET) == cursor);
    close_fd(first);
    first = -1;
    CHECK(syscall(SYS_write, shared, "YZ", 2) == 2
              && syscall(SYS_pread64, original, bytes, 2, 6) == 2
              && memcmp(bytes, "YZ", 2) == 0,
          "retained dup continues writing after the original fd closes");
out:
    if (mapped != MAP_FAILED)
        syscall(SYS_munmap, mapped, mapping_len);
    close_fd(shared);
    close_fd(second);
    close_fd(first);
    if (changed)
        CHECK(syscall(SYS_pwrite64, original, saved, length, 0) == length,
              "restore original data after the CACHED writer journey");
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
    /* Fresh symlink atime equal to ctime may require an RCU exit on Linux. */
    REQUIRE(syscall(SYS_mount, "none", "mount", "tmpfs", MS_NOATIME, NULL) == 0);
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
    path_commands(cached, "CACHED O_PATH file");
    errno = 0;
    CHECK(syscall(SYS_fcntl, -1, 0x7fffffff, 0) == -1 && errno == EBADF,
          "fcntl validates a missing descriptor before an unknown command errno=%d", errno);
    errno = 0;
    CHECK(syscall(SYS_fcntl, -1, 1035, 0) == -1 && errno == EBADF,
          "fcntl validates a missing descriptor before hint user memory errno=%d", errno);
    errno = 0;
    CHECK(syscall(SYS_fcntl, file, 0x7fffffff, 0) == -1 && errno == EINVAL,
          "ordinary file keeps unsupported command EINVAL errno=%d", errno);
    cached_readonly(mountfd, file);
    cached_write_completion(mountfd, file);
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

static void native_direct_write_progress(void)
{
    char directory[] = "/var/tmp/direct-write-progress-XXXXXX";
    const char *leaf = "contents";
    int root = -1, file = -1, reader = -1;
    int created = 0, file_created = 0;
    long page = sysconf(_SC_PAGESIZE);
    size_t span = 0;
    void *input = MAP_FAILED, *output = MAP_FAILED;
    struct stat status;
    struct statfs backend;

    REQUIRE(page > 0 && (size_t)page <= SIZE_MAX / 2);
    span = 2 * (size_t)page;
    REQUIRE(mkdtemp(directory) != NULL);
    created = 1;
    root = open_at(AT_FDCWD, directory, O_RDONLY | O_DIRECTORY);
    REQUIRE(root >= 0);
    REQUIRE(fstatfs(root, &backend) == 0);
    REQUIRE((unsigned long)backend.f_type == 0xef53);
    file = open_at(root, leaf, O_CREAT | O_EXCL | O_RDWR | O_DIRECT);
    REQUIRE(file >= 0);
    file_created = 1;
    input = mmap(NULL, span, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    REQUIRE(input != MAP_FAILED);
    output = mmap(NULL, span, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    REQUIRE(output != MAP_FAILED);
    memset(input, 0x51, (size_t)page);
    REQUIRE(mprotect((char *)input + page, (size_t)page, PROT_NONE) == 0);
    REQUIRE(syscall(SYS_pwrite64, file, input, (size_t)page, (off_t)0) == page);
    REQUIRE(syscall(SYS_ftruncate, file, (off_t)0) == 0);
    REQUIRE(syscall(SYS_fsync, file) == 0);

    errno = 0;
    long count = syscall(SYS_pwrite64, file, input, span, (off_t)0);
    int error = errno;
    CHECK(count == -1 && error == EFAULT,
          "native direct positioned write reports input preparation failure");
    REQUIRE(fstat(file, &status) == 0);
    CHECK(status.st_size == 0,
          "native direct positioned input failure preserves accepted length");
    CHECK(syscall(SYS_lseek, file, (off_t)0, SEEK_CUR) == 0,
          "native direct positioned write preserves description position");
    close_fd(file);
    file = open_at(root, leaf, O_RDWR | O_DIRECT | O_APPEND);
    REQUIRE(file >= 0);
    memset(input, 0x52, (size_t)page);
    errno = 0;
    count = syscall(SYS_write, file, input, span);
    error = errno;
    CHECK(count == -1 && error == EFAULT,
          "native direct append reports input preparation failure");
    REQUIRE(fstat(file, &status) == 0);
    CHECK(status.st_size == 0,
          "native direct append input failure preserves accepted length");
    CHECK(syscall(SYS_lseek, file, (off_t)0, SEEK_CUR) == 0,
          "native direct append input failure preserves description position");
    REQUIRE(syscall(SYS_ftruncate, file, (off_t)0) == 0);
    memset(input, 0x51, (size_t)page);
    REQUIRE(syscall(SYS_pwrite64, file, input, (size_t)page, (off_t)0) == page);
    memset(input, 0x52, (size_t)page);
    REQUIRE(syscall(SYS_write, file, input, (size_t)page) == page);
    REQUIRE(fstat(file, &status) == 0);
    CHECK(status.st_size == (off_t)span,
          "native direct valid writes publish accepted length after rejection");
    REQUIRE(syscall(SYS_fsync, file) == 0);
    reader = open_at(root, leaf, O_RDONLY | O_DIRECT);
    REQUIRE(reader >= 0);
    CHECK(syscall(SYS_pread64, reader, output, span, (off_t)0) == (long)span,
          "native direct write progress remains readable after synchronization");
    int contents_match = 1;
    for (size_t i = 0; i < span; i++) {
        unsigned char expected = i < (size_t)page ? 0x51 : 0x52;
        if (((unsigned char *)output)[i] != expected) {
            contents_match = 0;
            break;
        }
    }
    CHECK(contents_match, "native direct write progress preserves initialized contents");
out:
    close_fd(reader);
    close_fd(file);
    if (input != MAP_FAILED)
        CHECK(munmap(input, span) == 0, "release native direct write input");
    if (output != MAP_FAILED)
        CHECK(munmap(output, span) == 0, "release native direct write output");
    if (file_created)
        CHECK(unlinkat(root, leaf, 0) == 0, "remove native direct write fixture");
    close_fd(root);
    if (created)
        CHECK(rmdir(directory) == 0, "remove native direct write directory");
}

static void native_direct_write_mapped_input(void)
{
    char directory[] = "/var/tmp/direct-write-mapped-input-XXXXXX";
    int root = -1, file = -1;
    int created = 0, file_created = 0;
    long page = sysconf(_SC_PAGESIZE);
    size_t span = 0;
    void *initial = NULL, *output = NULL, *mapping = MAP_FAILED;
    struct stat status;
    struct statfs backend;

    REQUIRE(page > 0 && (size_t)page <= SIZE_MAX / 64);
    span = 32 * (size_t)page;
    REQUIRE(mkdtemp(directory) != NULL);
    created = 1;
    root = open_at(AT_FDCWD, directory, O_RDONLY | O_DIRECTORY);
    REQUIRE(root >= 0);
    REQUIRE(fstatfs(root, &backend) == 0);
    REQUIRE((unsigned long)backend.f_type == 0xef53);
    file = open_at(root, "contents", O_RDWR | O_CREAT | O_EXCL | O_DIRECT);
    REQUIRE(file >= 0);
    file_created = 1;
    REQUIRE(posix_memalign(&initial, (size_t)page, span) == 0);
    REQUIRE(posix_memalign(&output, (size_t)page, 2 * span) == 0);
    memset(initial, 0x6d, span);
    REQUIRE(syscall(SYS_pwrite64, file, initial, span, (off_t)0) == (long)span);
    mapping = (void *)syscall(SYS_mmap, NULL, span, PROT_READ, MAP_PRIVATE, file, 0);
    REQUIRE(mapping != MAP_FAILED);
    CHECK(syscall(SYS_pwrite64, file, mapping, span, (off_t)span) == (long)span,
          "native direct write accepts lazy input from the same file");
    REQUIRE(syscall(SYS_fstat, file, &status) == 0);
    CHECK(status.st_size == (off_t)(2 * span), "native mapped input publishes the complete length");
    REQUIRE(syscall(SYS_pread64, file, output, 2 * span, (off_t)0) == (long)(2 * span));
    CHECK(memcmp(output, initial, span) == 0, "native mapped input preserves its source contents");
    CHECK(memcmp((char *)output + span, initial, span) == 0,
          "native mapped input reaches the backing file");
out:
    if (mapping != MAP_FAILED)
        CHECK(munmap(mapping, span) == 0, "release native mapped input");
    free(initial);
    free(output);
    close_fd(file);
    if (file_created)
        CHECK(unlinkat(root, "contents", 0) == 0, "remove native mapped input file");
    close_fd(root);
    if (created)
        CHECK(rmdir(directory) == 0, "remove native mapped input directory");
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
    native_direct_write_progress();
    native_direct_write_mapped_input();
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
