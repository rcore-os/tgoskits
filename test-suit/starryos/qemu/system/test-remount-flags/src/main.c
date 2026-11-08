#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/syscall.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>
#include <unistd.h>

#define BUF_SIZE 65536

static long remount_tmp(unsigned long flags) {
    return syscall(SYS_mount, NULL, "/tmp", NULL, MS_REMOUNT | flags, NULL);
}

static int verify_remount_readonly_transition(void) {
    const char *path = "/tmp/remount-readonly-transition";

    unlink(path);
    if (remount_tmp(MS_RDONLY) < 0) {
        perror("remount /tmp readonly");
        return 1;
    }

    errno = 0;
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (fd >= 0) {
        close(fd);
        fprintf(stderr, "FAIL: readonly remount still allowed file creation\n");
        return 1;
    }
    if (errno != EROFS) {
        fprintf(stderr, "FAIL: readonly write errno=%d, expected EROFS\n", errno);
        return 1;
    }

    if (remount_tmp(0) < 0) {
        perror("remount /tmp read-write");
        return 1;
    }

    fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (fd < 0) {
        perror("create file after read-write remount");
        return 1;
    }
    close(fd);
    unlink(path);
    return 0;
}

static const char promotion_payload[] = "persisted after readonly promotion";

/* The same permission transition applies to a boot root and a fresh auxiliary
 * mount, regardless of filesystem. Check both the result and subsequent I/O. */
static int verify_readonly_promotion(const char *target, const char *path,
                                     int physically_readonly) {
    char contents[sizeof(promotion_payload)] = {0};
    errno = 0;
    int fd = syscall(SYS_openat, AT_FDCWD, path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (fd >= 0 || errno != EROFS) {
        if (fd >= 0) close(fd);
        fprintf(stderr, "FAIL: initial readonly mount accepted writes errno=%d\n", errno);
        return 1;
    }
    errno = 0;
    long result = syscall(SYS_mount, NULL, target, NULL, MS_REMOUNT, NULL);
    if (physically_readonly) {
        if (result != -1 || errno != EACCES) {
            fprintf(stderr, "FAIL: physical readonly promotion errno=%d, expected EACCES\n", errno);
            return 1;
        }
        errno = 0;
        fd = syscall(SYS_openat, AT_FDCWD, path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
        if (fd >= 0 || errno != EROFS) {
            if (fd >= 0) close(fd);
            fprintf(stderr, "FAIL: failed promotion changed write protection\n");
            return 1;
        }
        return 0;
    }
    if (result < 0) {
        perror("promote readonly filesystem on writable media");
        return 1;
    }
    /* Repeated RW remounts and flag-only changes must preserve write access. */
    if (syscall(SYS_mount, NULL, target, NULL, MS_REMOUNT, NULL) < 0
        || syscall(SYS_mount, NULL, target, NULL, MS_REMOUNT | MS_NOSUID, NULL) < 0) {
        perror("remount an already writable filesystem");
        return 1;
    }
    fd = syscall(SYS_openat, AT_FDCWD, path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (fd < 0 || syscall(SYS_write, fd, promotion_payload, sizeof(promotion_payload))
                      != (ssize_t)sizeof(promotion_payload)
        || syscall(SYS_fsync, fd) < 0) {
        if (fd >= 0) close(fd);
        perror("persist file after writable promotion");
        return 1;
    }
    close(fd);
    if (syscall(SYS_mount, NULL, target, NULL, MS_REMOUNT | MS_RDONLY, NULL) < 0) {
        perror("restore readonly filesystem");
        return 1;
    }
    fd = syscall(SYS_openat, AT_FDCWD, path, O_RDONLY, 0);
    if (fd < 0 || syscall(SYS_read, fd, contents, sizeof(contents)) != (ssize_t)sizeof(contents)
        || memcmp(contents, promotion_payload, sizeof(promotion_payload)) != 0) {
        if (fd >= 0) close(fd);
        fprintf(stderr, "FAIL: writable promotion did not preserve file contents\n");
        return 1;
    }
    close(fd);
    return 0;
}

/* The dedicated QEMU profile supplies a real readonly VirtIO block backend.
 * Other profiles still exercise the existing tmpfs transition below. */
static int verify_physical_readonly_mount(void) {
    const char *device = getenv("STARRY_PHYSICAL_READONLY_DEVICE");
    const char *target = "/tmp/remount-physical-readonly";
    const char *path = "/tmp/remount-physical-readonly/must-not-exist";
    if (device == NULL) {
        return 0;
    }
    /* Device identity is the contract here; libc selects the architecture's
     * stat syscall and converts its ABI into struct stat. */
    struct stat root_stat, device_stat;
    if (fstatat(AT_FDCWD, "/", &root_stat, 0) < 0
        || fstatat(AT_FDCWD, device, &device_stat, 0) < 0) {
        perror("query physical root and VirtIO identity");
        return 1;
    }
    if (major(root_stat.st_dev) != 259 || minor(root_stat.st_dev) != 0
        || major(device_stat.st_rdev) != 252 || minor(device_stat.st_rdev) != 0) {
        fprintf(stderr, "FAIL: root=%u:%u readonly-device=%u:%u identity mismatch\n",
                major(root_stat.st_dev), minor(root_stat.st_dev),
                major(device_stat.st_rdev), minor(device_stat.st_rdev));
        return 1;
    }
    FILE *diskstats = fopen("/proc/diskstats", "r");
    if (diskstats == NULL) {
        perror("read diskstats");
        return 1;
    }
    int root_named = 0;
    char stats_line[1024];
    while (fgets(stats_line, sizeof(stats_line), diskstats) != NULL) {
        unsigned int device_major, device_minor;
        char name[128];
        if (sscanf(stats_line, "%u %u %127s", &device_major, &device_minor, name) == 3) {
            root_named |= device_major == 259 && device_minor == 0 && strcmp(name, "nvme0n1") == 0;
        }
    }
    fclose(diskstats);
    if (!root_named) {
        fprintf(stderr, "FAIL: root device identity disagrees with diskstats\n");
        return 1;
    }
    if (mkdir(target, 0700) < 0) {
        perror("create physical mountpoint");
        return 1;
    }
    int failed = 0;
    errno = 0;
    if (syscall(SYS_mount, device, target, "ext4", 0, NULL) != -1 || errno != EACCES) {
        fprintf(stderr, "FAIL: physical readonly RW mount errno=%d, expected EACCES\n", errno);
        failed = 1;
    }
    if (syscall(SYS_mount, device, target, "ext4", MS_RDONLY | MS_NOSUID, NULL) < 0) {
        perror("mount physical readonly ext4");
        return 1;
    }
    if (verify_readonly_promotion(target, path, 1) != 0) failed = 1;
    FILE *mounts = fopen("/proc/self/mounts", "r");
    if (mounts == NULL) {
        perror("read mounts after failed promotion");
        return 1;
    }
    char line[1024];
    int unchanged = 0;
    while (fgets(line, sizeof(line), mounts) != NULL) {
        char source[256], mountpoint[256], type[64], options[256];
        if (sscanf(line, "%255s %255s %63s %255s", source, mountpoint, type, options) == 4
            && strcmp(mountpoint, target) == 0) {
            int ro = 0, nosuid = 0;
            char *save = NULL;
            for (char *flag = strtok_r(options, ",", &save); flag != NULL;
                 flag = strtok_r(NULL, ",", &save)) {
                ro |= strcmp(flag, "ro") == 0;
                nosuid |= strcmp(flag, "nosuid") == 0;
            }
            unchanged = ro && nosuid;
        }
    }
    fclose(mounts);
    if (!unchanged) {
        fprintf(stderr, "FAIL: failed promotion changed mount flags\n");
        failed = 1;
    }
    if (syscall(SYS_umount2, target, 0) < 0 || rmdir(target) < 0) {
        perror("release physical readonly mount");
        return 1;
    }
    if (failed) return 1;
    puts("PHYSICAL_READONLY_MOUNT_PASSED");
    return 0;
}

/* Remount updates the shared VFS superblock state. Native mount reuse must
 * consult that live policy, including when the underlying owner stays writable. */
static int verify_native_superblock_policy(void) {
    if (getenv("STARRY_PHYSICAL_READONLY_DEVICE") == NULL) return 0;
    const char *device = "/dev/nvme0n1";
    const char *ro_target = "/tmp/remount-native-ro";
    const char *rw_target = "/tmp/remount-native-rw";
    int failed = 0, root_readonly = 0, ro_mounted = 0, rw_mounted = 0;
    if (mkdir(ro_target, 0700) < 0 || mkdir(rw_target, 0700) < 0) {
        perror("create native reuse mountpoints");
        failed = 1;
        goto cleanup;
    }
    if (syscall(SYS_mount, NULL, "/", NULL, MS_REMOUNT | MS_RDONLY, NULL) < 0) {
        perror("remount native root readonly");
        failed = 1;
        goto cleanup;
    }
    root_readonly = 1;
    if (syscall(SYS_mount, device, ro_target, "ext4", MS_RDONLY, NULL) < 0) {
        fprintf(stderr, "FAIL: readonly native superblock reuse errno=%d, expected success\n", errno);
        failed = 1;
    } else {
        ro_mounted = 1;
    }
    errno = 0;
    if (syscall(SYS_mount, device, rw_target, "ext4", 0, NULL) != -1) {
        rw_mounted = 1;
        fprintf(stderr, "FAIL: native RW mount reused readonly superblock, expected EBUSY\n");
        failed = 1;
    } else if (errno != EBUSY) {
        fprintf(stderr, "FAIL: native RW reuse errno=%d, expected EBUSY\n", errno);
        failed = 1;
    }
cleanup:
    if (rw_mounted && syscall(SYS_umount2, rw_target, 0) < 0) {
        perror("unmount unexpected writable alias");
        failed = 1;
    }
    if (ro_mounted && syscall(SYS_umount2, ro_target, 0) < 0) {
        perror("unmount readonly alias");
        failed = 1;
    }
    if (root_readonly && syscall(SYS_mount, NULL, "/", NULL, MS_REMOUNT, NULL) < 0) {
        perror("restore native writable root");
        failed = 1;
    }
    rmdir(ro_target);
    rmdir(rw_target);
    if (!failed) puts("NATIVE_SUPERBLOCK_REUSE_PASSED");
    return failed;
}

/* Start with a fresh readonly superblock on writable media, rather than
 * reusing the writable root's backend and only changing VFS flags. */
static int verify_writable_backend_promotion(void) {
    const char *device = getenv("STARRY_WRITABLE_REMOUNT_DEVICE");
    const char *target = "/tmp/remount-writable-media";
    const char *path = "/tmp/remount-writable-media/remount-payload";
    char contents[sizeof(promotion_payload)] = {0};
    int failed = 0, mounted = 0, fd = -1;
    if (device == NULL) return 0;
    if (mkdir(target, 0700) < 0) {
        perror("create writable-media mountpoint");
        return 1;
    }
    if (syscall(SYS_mount, device, target, "ext4", MS_RDONLY, NULL) < 0) {
        perror("initial readonly mount on writable media");
        failed = 1;
        goto cleanup;
    }
    mounted = 1;
    if (verify_readonly_promotion(target, path, 0) != 0) {
        failed = 1;
        goto cleanup;
    }
    if (syscall(SYS_umount2, target, 0) < 0) {
        perror("unmount promoted ext4");
        failed = 1;
        goto cleanup;
    }
    mounted = 0;
    if (syscall(SYS_mount, device, target, "ext4", MS_RDONLY, NULL) < 0) {
        perror("reopen promoted ext4 readonly");
        failed = 1;
        goto cleanup;
    }
    mounted = 1;
    fd = open(path, O_RDONLY);
    if (fd < 0 || read(fd, contents, sizeof(contents)) != (ssize_t)sizeof(contents)
        || memcmp(contents, promotion_payload, sizeof(promotion_payload)) != 0) {
        fprintf(stderr, "FAIL: writable promotion did not preserve the file\n");
        failed = 1;
    }
cleanup:
    if (fd >= 0) close(fd);
    if (mounted && syscall(SYS_umount2, target, 0) < 0) {
        perror("release writable-media mount");
        failed = 1;
    }
    if (rmdir(target) < 0) failed = 1;
    if (!failed) puts("WRITABLE_BACKEND_PROMOTION_PASSED");
    return failed;
}

int main(int argc, char **argv) {
    if (argc == 2 && (strcmp(argv[1], "--readonly-root") == 0
                     || strcmp(argv[1], "--physical-readonly-root") == 0)) {
        int physically_readonly = strcmp(argv[1], "--physical-readonly-root") == 0;
        if (verify_readonly_promotion("/", "/remount-payload", physically_readonly) != 0) return 1;
        puts(physically_readonly ? "PHYSICAL_READONLY_ROOT_PASSED" : "READONLY_ROOT_PROMOTION_PASSED");
        fflush(stdout);
        /* Match the normal runner's optional instrumented-kernel handshake. */
        int coverage = syscall(SYS_openat, AT_FDCWD, "/proc/starry-test-coverage", O_WRONLY, 0);
        if (coverage >= 0) {
            if (syscall(SYS_write, coverage, "1", 1) != 1) {
                close(coverage);
                fprintf(stderr, "FAIL: export root mount coverage\n");
                return 1;
            }
            close(coverage);
        } else if (errno != ENOENT) {
            perror("open root mount coverage control");
            return 1;
        }
        /* The runner terminates QEMU on the success marker. PID 1 must not
         * exit and turn a successful check into an unrelated init panic. */
        for (;;) pause();
    }
    if (verify_physical_readonly_mount() != 0 || verify_native_superblock_policy() != 0
        || verify_writable_backend_promotion() != 0
        || verify_remount_readonly_transition() != 0) {
        return 1;
    }

    /* Remount /tmp with nosuid */
    if (remount_tmp(MS_NOSUID) < 0) {
        perror("remount /tmp nosuid");
        return 1;
    }

    /* Read /proc/self/mountinfo and check /tmp entry has nosuid */
    FILE *f = fopen("/proc/self/mountinfo", "r");
    if (!f) {
        fprintf(stderr, "FAIL: cannot open /proc/self/mountinfo\n");
        return 1;
    }

    char buf[BUF_SIZE];
    size_t n = fread(buf, 1, sizeof(buf) - 1, f);
    fclose(f);
    buf[n] = '\0';

    /* Find the /tmp line */
    char *saveptr = NULL;
    char *line = strtok_r(buf, "\n", &saveptr);
    int found = 0;

    while (line) {
        long mount_id, parent_id;
        int major, minor;
        char root[256], mount_point[256], options[256];
        char optional[256], fstype[64], source[64], super_opts[256];

        int fields = sscanf(line, "%ld %ld %d:%d %255s %255s %255s %255s - %63s %63s %255s",
                            &mount_id, &parent_id, &major, &minor,
                            root, mount_point, options, optional,
                            fstype, source, super_opts);

        if (fields >= 7 && strcmp(mount_point, "/tmp") == 0) {
            found = 1;
            if (strstr(options, "nosuid") == NULL) {
                fprintf(stderr, "FAIL: /tmp options='%s' missing nosuid\n", options);
                return 1;
            }
            break;
        }

        line = strtok_r(NULL, "\n", &saveptr);
    }

    if (!found) {
        fprintf(stderr, "FAIL: /tmp not found in mountinfo\n");
        return 1;
    }

    printf("TEST_REMOUNT_FLAGS_PASSED\n");
    return 0;
}
