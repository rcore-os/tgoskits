#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <sys/mount.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/socket.h>
#include <sys/statvfs.h>
#include <sys/syscall.h>
#include <sys/vfs.h>
#include <stdint.h>
#include <unistd.h>

static int passed;
static int failed;

static void expect_true(int condition, const char *name)
{
    if (condition) {
        printf("PASS: %s\n", name);
        passed++;
        return;
    }
    printf("FAIL: %s: errno=%d (%s)\n", name, errno, strerror(errno));
    failed++;
}

/* The path and descriptor must observe one published root geometry, including
 * when boot preparation has expanded the underlying ext4 filesystem. */
static void verify_root_filesystem_statistics(void)
{
    struct statfs by_path, by_fd;
    int root_fd = open("/", O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    expect_true(root_fd >= 0, "open root directory fd");
    if (root_fd < 0) {
        return;
    }
    int path_result = syscall(SYS_statfs, "/", &by_path);
    expect_true(path_result == 0, "statfs reports root filesystem");
    int fd_result = syscall(SYS_fstatfs, root_fd, &by_fd);
    expect_true(fd_result == 0, "fstatfs reports root filesystem");
    close(root_fd);
    if (path_result != 0 || fd_result != 0) {
        return;
    }
    expect_true(by_path.f_type == by_fd.f_type
                && by_path.f_bsize == by_fd.f_bsize
                && by_path.f_blocks == by_fd.f_blocks,
                "root path and fd report the same geometry");
    /* Free counters are independent snapshots; another process may allocate
     * between these calls without changing the published filesystem geometry. */
    expect_true(by_path.f_bsize > 0 && by_path.f_blocks > 0
                && by_path.f_bfree <= by_path.f_blocks
                && by_path.f_bavail <= by_path.f_bfree,
                "root filesystem capacity is consistent");
    expect_true(by_fd.f_bsize > 0 && by_fd.f_blocks > 0
                && by_fd.f_bfree <= by_fd.f_blocks
                && by_fd.f_bavail <= by_fd.f_bfree,
                "root fd capacity is consistent");
    if (by_path.f_bsize <= 0 || by_path.f_blocks > UINT64_MAX / by_path.f_bsize) {
        expect_true(0, "root filesystem byte size is representable");
        return;
    }
    const uint64_t capacity = (uint64_t)by_path.f_blocks * by_path.f_bsize;
    printf("STARRY_ROOT_STATFS_BYTES=%llu\n", (unsigned long long)capacity);
    const char *expected = getenv("STARRY_ROOT_EXPECTED_BYTES");
    if (expected != NULL) {
        char *end;
        errno = 0;
        unsigned long long target = strtoull(expected, &end, 10);
        expect_true(errno == 0 && *expected != '\0' && *end == '\0'
                    && target > 0 && capacity == target,
                    "boot growth publishes the enlarged root capacity");
    }
}

/* All queries use the retained description's actual filesystem, including
 * native objects whose display names cannot be looked up in the mount tree. */
static void verify_native_descriptor(int fd, uint64_t type, const char *label)
{
    char name[128], path[64];
    snprintf(name, sizeof(name), "create %s descriptor", label);
    expect_true(fd >= 0, name);
    if (fd < 0) return;
    struct statfs by_fd, by_path, by_copy;
    errno = 0;
    int fd_result = syscall(SYS_fstatfs, fd, &by_fd);
    snprintf(name, sizeof(name), "fstatfs accepts %s descriptor", label);
    expect_true(fd_result == 0, name);
    if (fd_result == 0) {
        snprintf(name, sizeof(name), "%s descriptor reports its native filesystem", label);
        expect_true((uint64_t)by_fd.f_type == type && by_fd.f_bsize > 0, name);
    }
    snprintf(path, sizeof(path), "/proc/self/fd/%d", fd);
    int path_result = syscall(SYS_statfs, path, &by_path);
    snprintf(name, sizeof(name), "statfs follows %s object link", label);
    expect_true(path_result == 0, name);
    if (path_result == 0 && type != 0x01021994) {
        snprintf(name, sizeof(name), "%s pseudo filesystem has page geometry and no block capacity", label);
        expect_true((uint64_t)by_path.f_type == type &&
                    (uint64_t)by_path.f_bsize == (uint64_t)sysconf(_SC_PAGESIZE) &&
                    by_path.f_blocks == 0 && by_path.f_bfree == 0 &&
                    by_path.f_bavail == 0 && by_path.f_files == 0 && by_path.f_ffree == 0, name);
    }
    if (fd_result == 0 && path_result == 0) {
        snprintf(name, sizeof(name), "%s path and descriptor share filesystem identity", label);
        expect_true(by_fd.f_type == by_path.f_type &&
                    by_fd.f_bsize == by_path.f_bsize &&
                    memcmp(&by_fd.f_fsid, &by_path.f_fsid, sizeof(by_fd.f_fsid)) == 0, name);
    }
    int copy = syscall(SYS_fcntl, fd, F_DUPFD_CLOEXEC, 0);
    snprintf(name, sizeof(name), "retain %s description before original close", label);
    expect_true(copy >= 0, name);
    close(fd);
    if (copy >= 0) {
        int copied_result = syscall(SYS_fstatfs, copy, &by_copy);
        snprintf(name, sizeof(name), "%s filesystem survives closing the original fd", label);
        expect_true(copied_result == 0 && (uint64_t)by_copy.f_type == type, name);
        close(copy);
    }
}

static void verify_native_object_statistics(void)
{
    int endpoints[2];
    if (pipe2(endpoints, O_CLOEXEC) != 0) {
        expect_true(0, "create native pipe fixture");
    } else {
        verify_native_descriptor(endpoints[0], 0x50495045, "pipe reader");
        verify_native_descriptor(endpoints[1], 0x50495045, "pipe writer");
    }
    verify_native_descriptor(socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0),
                             0x534f434b, "Unix socket");
    verify_native_descriptor(socket(AF_INET, SOCK_DGRAM | SOCK_CLOEXEC, 0),
                             0x534f434b, "Internet socket");
    verify_native_descriptor(open("/proc/self/ns/mnt", O_RDONLY | O_CLOEXEC),
                             0x6e736673, "mount namespace");
    verify_native_descriptor(syscall(SYS_memfd_create, "native-statfs", 1),
                             0x01021994, "memfd");
}

static int same_geometry(const struct statfs *a, const struct statfs *b)
{
    return a->f_bsize == b->f_bsize && a->f_frsize == b->f_frsize &&
           a->f_blocks == b->f_blocks;
}

static void verify_overlay_statistics(const char *directory)
{
    char lower[160], upper[160], work[160], merged[160], options[600], work_child[180];
    snprintf(lower, sizeof(lower), "%s/lower", directory);
    snprintf(upper, sizeof(upper), "%s/upper", directory);
    snprintf(work, sizeof(work), "%s/work", directory);
    snprintf(merged, sizeof(merged), "%s/merged", directory);
    if (mkdir(lower, 0700) != 0 || mkdir(upper, 0700) != 0 ||
        mkdir(work, 0700) != 0 || mkdir(merged, 0700) != 0) {
        expect_true(0, "SETUP Overlay statistics directories");
        return;
    }
    struct statfs upper_stats, lower_stats, merged_stats;
    if (syscall(SYS_statfs, upper, &upper_stats) != 0 ||
        syscall(SYS_statfs, "/bin", &lower_stats) != 0) {
        expect_true(0, "SETUP Overlay backing filesystem statistics");
        goto cleanup;
    }
    expect_true(!same_geometry(&upper_stats, &lower_stats),
                "SETUP Overlay layers have distinguishable filesystem geometry");
    for (int writable = 1; writable >= 0; --writable) {
        if (writable)
            snprintf(options, sizeof(options), "lowerdir=/bin:%s,upperdir=%s,workdir=%s,index=off", lower, upper, work);
        else
            snprintf(options, sizeof(options), "lowerdir=/bin:%s", lower);
        if (mount("overlay", merged, "overlay", 0, options) != 0) {
            expect_true(0, writable ? "mount writable Overlay statistics fixture"
                                    : "mount lower-only Overlay statistics fixture");
            break;
        }
        int result = syscall(SYS_statfs, merged, &merged_stats);
        expect_true(result == 0, "statfs reports Overlay filesystem");
        if (result == 0) {
            expect_true(merged_stats.f_type == 0x794c7630,
                        "Overlay reports its own filesystem type");
            expect_true(same_geometry(&merged_stats, writable ? &upper_stats : &lower_stats),
                        writable ? "Overlay capacity comes from the actual upper filesystem"
                                 : "lower-only Overlay capacity comes from the actual lower filesystem");
            if (!writable)
                expect_true(merged_stats.f_flags & ST_RDONLY,
                            "lower-only Overlay statistics remain read-only");
        }
        if (umount2(merged, 0) != 0) {
            expect_true(0, "unmount Overlay after statistics queries");
            return;
        }
    }
cleanup:
    /* Linux keeps an empty internal work directory after a read-only query. */
    snprintf(work_child, sizeof(work_child), "%s/work", work);
    if (rmdir(work_child) != 0 && errno != ENOENT)
        expect_true(0, "clean Overlay internal work directory");
    if (rmdir(merged) != 0 || rmdir(work) != 0 || rmdir(upper) != 0 || rmdir(lower) != 0)
        expect_true(0, "clean Overlay statistics directories");
}

int main(void)
{
    const char *directory_path = "/tmp/bugfix-fstatvfs-directory";
    const char *file_path = "/tmp/bugfix-fstatvfs-directory/file";
    struct statvfs filesystem;

    printf("=== bugfix-fstatvfs-directory ===\n");
    verify_root_filesystem_statistics();
    verify_native_object_statistics();
    mkdir(directory_path, 0700);
    verify_overlay_statistics(directory_path);

    int directory_fd = open(directory_path, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    expect_true(directory_fd >= 0, "open directory fd");
    if (directory_fd >= 0) {
        errno = 0;
        int result = fstatvfs(directory_fd, &filesystem);
        expect_true(result == 0, "fstatvfs accepts directory fd");
        if (result == 0) {
            expect_true(filesystem.f_bsize > 0,
                        "directory fstatvfs reports block size");
        }
        close(directory_fd);
    }

    int file_fd = open(file_path, O_CREAT | O_RDWR | O_TRUNC | O_CLOEXEC, 0600);
    expect_true(file_fd >= 0, "open regular file fd");
    if (file_fd >= 0) {
        errno = 0;
        expect_true(fstatvfs(file_fd, &filesystem) == 0,
                    "fstatvfs accepts regular file fd");
        close(file_fd);
    }

    errno = 0;
    expect_true(fstatvfs(-1, &filesystem) == -1 && errno == EBADF,
                "fstatvfs rejects invalid fd with EBADF");

    unlink(file_path);
    rmdir(directory_path);

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        puts("STARRY_FSTATVFS_DIRECTORY_PASSED");
        puts("STARRY_GROUPED_TEST_PASSED: bugfix-fstatvfs-directory");
        return EXIT_SUCCESS;
    }
    puts("STARRY_GROUPED_TEST_FAILED: bugfix-fstatvfs-directory");
    return EXIT_FAILURE;
}
