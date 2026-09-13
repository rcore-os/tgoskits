#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef AT_NO_AUTOMOUNT
#define AT_NO_AUTOMOUNT 0x800
#endif
#ifndef AT_STATX_DONT_SYNC
#define AT_STATX_DONT_SYNC 0x4000
#endif
#ifndef STATX_TYPE
#define STATX_TYPE 0x00000001U
#endif
#ifndef STATX_INO
#define STATX_INO 0x00000100U
#endif
#ifndef STATX_MNT_ID
#define STATX_MNT_ID 0x00001000U
#endif
#ifndef STATX_ATTR_MOUNT_ROOT
#define STATX_ATTR_MOUNT_ROOT 0x00002000ULL
#endif

struct statx_timestamp_abi {
    int64_t tv_sec;
    uint32_t tv_nsec;
    int32_t reserved;
};

struct statx_abi {
    uint32_t stx_mask;
    uint32_t stx_blksize;
    uint64_t stx_attributes;
    uint32_t stx_nlink;
    uint32_t stx_uid;
    uint32_t stx_gid;
    uint16_t stx_mode;
    uint16_t pad0;
    uint64_t stx_ino;
    uint64_t stx_size;
    uint64_t stx_blocks;
    uint64_t stx_attributes_mask;
    struct statx_timestamp_abi stx_atime;
    struct statx_timestamp_abi stx_btime;
    struct statx_timestamp_abi stx_ctime;
    struct statx_timestamp_abi stx_mtime;
    uint32_t stx_rdev_major;
    uint32_t stx_rdev_minor;
    uint32_t stx_dev_major;
    uint32_t stx_dev_minor;
    uint64_t stx_mnt_id;
    uint64_t spare[13];
};

struct file_handle_abi {
    unsigned int handle_bytes;
    int handle_type;
    unsigned char bytes[128];
};

static int failures;

static void check(int condition, const char *message)
{
    if (condition) {
        printf("PASS: %s\n", message);
        return;
    }

    fprintf(stderr, "FAIL: %s: errno=%d (%s)\n", message, errno,
            strerror(errno));
    failures++;
}

static void check_mount_root(const char *path, int expected)
{
    struct statx_abi status;
    memset(&status, 0, sizeof(status));

    errno = 0;
    long result = syscall(SYS_statx, AT_FDCWD, path,
                          AT_NO_AUTOMOUNT | AT_STATX_DONT_SYNC,
                          STATX_TYPE | STATX_INO, &status);

    char message[160];
    snprintf(message, sizeof(message), "statx(2) succeeds for %s", path);
    check(result == 0, message);
    if (result != 0) {
        return;
    }

    snprintf(message, sizeof(message),
             "statx(2) reports STATX_ATTR_MOUNT_ROOT support for %s", path);
    check((status.stx_attributes_mask & STATX_ATTR_MOUNT_ROOT) != 0,
          message);

    snprintf(message, sizeof(message),
             "statx(2) reports the expected mount-root state for %s", path);
    check(((status.stx_attributes & STATX_ATTR_MOUNT_ROOT) != 0) == expected,
          message);
}

static uint64_t check_mount_id(const char *path)
{
    struct statx_abi status;
    memset(&status, 0, sizeof(status));

    errno = 0;
    long result = syscall(SYS_statx, AT_FDCWD, path,
                          AT_NO_AUTOMOUNT | AT_STATX_DONT_SYNC,
                          STATX_TYPE | STATX_INO | STATX_MNT_ID, &status);

    char message[160];
    snprintf(message, sizeof(message), "statx(2) returns a mount ID for %s",
             path);
    check(result == 0 && (status.stx_mask & STATX_MNT_ID) != 0 &&
              status.stx_mnt_id != 0,
          message);
    return status.stx_mnt_id;
}

static void check_name_to_handle_mount_id(const char *path,
                                          uint64_t expected_mount_id)
{
    struct file_handle_abi handle;
    int mount_id = 0;
    memset(&handle, 0, sizeof(handle));
    handle.handle_bytes = sizeof(handle.bytes);

    errno = 0;
    long result = syscall(SYS_name_to_handle_at, AT_FDCWD, path, &handle,
                          &mount_id, 0);

    char message[192];
    snprintf(message, sizeof(message),
             "name_to_handle_at(2) mount ID matches statx(2) for %s", path);
    check(result == 0 && mount_id > 0 &&
              (uint64_t)mount_id == expected_mount_id,
          message);
}

int main(void)
{
    check_mount_root("/", 1);
    check_mount_root("/proc", 1);
    check_mount_root("/proc/1", 0);
    check_mount_root("/run", 0);

    uint64_t root_mount_id = check_mount_id("/");
    uint64_t proc_mount_id = check_mount_id("/proc");
    uint64_t proc_child_mount_id = check_mount_id("/proc/1");
    check(root_mount_id != 0 && proc_mount_id != 0 &&
              root_mount_id != proc_mount_id,
          "statx(2) distinguishes separate mounts");
    check(proc_mount_id != 0 && proc_mount_id == proc_child_mount_id,
          "statx(2) keeps one mount ID within a mount");
    check_name_to_handle_mount_id("/proc", proc_mount_id);

    if (failures != 0) {
        fprintf(stderr, "STARRY_STATX_MOUNT_ROOT_FAILED: %d checks\n",
                failures);
        return 1;
    }

    puts("STARRY_STATX_MOUNT_ROOT_PASSED");
    return 0;
}
