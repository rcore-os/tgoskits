#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include "test_framework.h"
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/mount.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <sys/syscall.h>
#include <unistd.h>

static int switch_user(uid_t uid, gid_t gid) {
    if (setresgid(gid, gid, (gid_t)0) != 0) {
        fprintf(stderr, "setresgid(%u) failed: %s\n", gid, strerror(errno));
        return -1;
    }
    if (setresuid(uid, uid, (uid_t)0) != 0) {
        fprintf(stderr, "setresuid(%u) failed: %s\n", uid, strerror(errno));
        return -1;
    }
    return 0;
}

static int check_owner(const char *path, uid_t expected_uid, gid_t expected_gid) {
    struct stat st;
    if (stat(path, &st) != 0) {
        fprintf(stderr, "stat(%s) failed: %s\n", path, strerror(errno));
        return -1;
    }
    if (st.st_uid != expected_uid) {
        fprintf(stderr, "%s: expected uid=%u got uid=%u\n",
                path, expected_uid, st.st_uid);
        return -1;
    }
    if (st.st_gid != expected_gid) {
        fprintf(stderr, "%s: expected gid=%u got gid=%u\n",
                path, expected_gid, st.st_gid);
        return -1;
    }
    return 0;
}

static int check_owner_fd(int fd, uid_t expected_uid, gid_t expected_gid) {
    struct stat st;
    if (fstat(fd, &st) != 0) {
        fprintf(stderr, "fstat(fd=%d) failed: %s\n", fd, strerror(errno));
        return -1;
    }
    if (st.st_uid != expected_uid) {
        fprintf(stderr, "fd=%d: expected uid=%u got uid=%u\n",
                fd, expected_uid, st.st_uid);
        return -1;
    }
    if (st.st_gid != expected_gid) {
        fprintf(stderr, "fd=%d: expected gid=%u got gid=%u\n",
                fd, expected_gid, st.st_gid);
        return -1;
    }
    return 0;
}

struct fs_identity_probe {
    int reader;
    uid_t uid;
    gid_t gid;
};

static void *observe_fs_identity(void *argument) {
    struct fs_identity_probe *probe = argument;
    char ready;
    if (syscall(SYS_read, probe->reader, &ready, 1) == 1) {
        probe->uid = syscall(SYS_setfsuid, (uid_t)-1);
        probe->gid = syscall(SYS_setfsgid, (gid_t)-1);
    }
    return NULL;
}

/* Pathname creation uses the original filesystem identity and socket mode. */
static void unix_ownership(void) {
    char scratch[] = "/tmp/vfs-unix-XXXXXX";
    char path[108];
    struct sockaddr_un address = { .sun_family = AF_UNIX };
    struct stat st;
    int bound = -1, peer = -1;
    int gate[2] = { -1, -1 }, started = 0;
    pthread_t reporter;
    struct fs_identity_probe probe = { .reader = -1, .uid = (uid_t)-1, .gid = (gid_t)-1 };
    mode_t old_mask = umask(0027);
    int workspace = mkdtemp(scratch) != NULL;
    CHECK(workspace, "create Unix socket ownership workspace");
    if (!workspace) goto cleanup;
    CHECK_RET(chown(scratch, 0, 90), 0, "set parent group for socket inheritance");
    CHECK_RET(chmod(scratch, 02777), 0, "enable socket parent SGID inheritance");
    snprintf(path, sizeof(path), "%s/socket", scratch);
    memcpy(address.sun_path, path, strlen(path) + 1);
    bound = syscall(SYS_socket, AF_UNIX, SOCK_DGRAM | SOCK_CLOEXEC, 0);
    peer = syscall(SYS_socket, AF_UNIX, SOCK_DGRAM | SOCK_CLOEXEC, 0);
    CHECK(bound >= 0 && peer >= 0, "create anonymous sockets before pathname binding");
    if (bound < 0 || peer < 0) goto cleanup;
    CHECK_RET(syscall(SYS_fchmod, bound, 0767), 0, "set mode on the actual anonymous socket inode");
    int ready = syscall(SYS_pipe2, gate, O_CLOEXEC) == 0;
    CHECK(ready, "prepare the sibling identity handoff");
    if (!ready) goto cleanup;
    probe.reader = gate[0];
    started = pthread_create(&reporter, NULL, observe_fs_identity, &probe) == 0;
    CHECK(started, "retain a sibling thread before changing filesystem identity");
    if (!started) goto cleanup;
    syscall(SYS_setfsgid, 88);
    syscall(SYS_setfsuid, 77);
    int result = syscall(SYS_bind, bound, &address, sizeof(address));
    int saved_errno = errno;
    CHECK_RET(syscall(SYS_write, gate[1], "x", 1), 1, "release sibling after caller identity publication");
    close(gate[1]); gate[1] = -1;
    int joined = pthread_join(reporter, NULL) == 0;
    CHECK(joined && probe.uid == 0 && probe.gid == 0,
          "raw filesystem ID setters leave the sibling thread identity unchanged");
    if (joined) started = 0;
    syscall(SYS_setfsuid, 0);
    syscall(SYS_setfsgid, 0);
    errno = saved_errno;
    CHECK_RET(result, 0, "bind with the original filesystem identity");
    if (result != 0) goto cleanup;
    CHECK(syscall(SYS_newfstatat, AT_FDCWD, path, &st, AT_SYMLINK_NOFOLLOW) == 0 &&
          S_ISSOCK(st.st_mode) && st.st_uid == 77 && st.st_gid == 90 &&
          (st.st_mode & 07777) == 0740,
          "pathname atomically inherits caller uid, parent gid and masked socket mode");
    close(bound); bound = -1;
    CHECK_ERR(syscall(SYS_bind, peer, &address, sizeof(address)), EADDRINUSE,
              "closing a socket retains its occupied pathname");
    CHECK_RET(syscall(SYS_unlinkat, AT_FDCWD, path, 0), 0, "unlink the retained pathname explicitly");
    CHECK_RET(chmod(scratch, 0500), 0, "deny caller access to the socket parent");
    syscall(SYS_setfsgid, 88);
    syscall(SYS_setfsuid, 77);
    result = syscall(SYS_bind, peer, &address, sizeof(address));
    saved_errno = errno;
    syscall(SYS_setfsuid, 0);
    syscall(SYS_setfsgid, 0);
    errno = saved_errno;
    CHECK(result == -1 && saved_errno == EACCES,
          "pathname binding cannot borrow the kernel root identity");
    CHECK_ERR(syscall(SYS_newfstatat, AT_FDCWD, path, &st, AT_SYMLINK_NOFOLLOW), ENOENT,
              "denied pathname binding publishes no inode");
cleanup:
    syscall(SYS_setfsuid, 0);
    syscall(SYS_setfsgid, 0);
    if (gate[1] >= 0) close(gate[1]);
    if (started) pthread_join(reporter, NULL);
    if (gate[0] >= 0) close(gate[0]);
    umask(old_mask);
    if (bound >= 0) close(bound);
    if (peer >= 0) close(peer);
    if (workspace) {
        snprintf(path, sizeof(path), "%s/socket", scratch);
        unlink(path);
        rmdir(scratch);
    }
}

/* This journey uses a real MemoryFs backing and real syscall entry points.
 * It shares the existing ownership process/boot and restores both mounts. */
static void overlay_ownership(void) {
    char scratch[] = "/tmp/vfs-overlay-XXXXXX";
    char lower[128], upper[128], work[128], merged[128];
    char source[256], path[256], other[256], options[768];
    unsigned char content[8193], observed[sizeof(content)];
    struct stat st;
    int fd = -1, orphan = -1, created = -1;
    int backing_mounted = 0, overlay_mounted = 0;
    for (size_t i = 0; i < sizeof(content); ++i) content[i] = (unsigned char)(i * 29 + 3);
#define OVL_REQUIRE(condition, message) do { \
    int ok = !!(condition); CHECK(ok, message); if (!ok) goto cleanup; \
} while (0)
    OVL_REQUIRE(mkdtemp(scratch) != NULL, "create isolated Overlay workspace");
    OVL_REQUIRE(syscall(SYS_mount, "tmpfs", scratch, "tmpfs", 0, "size=16777216") == 0,
                "mount native MemoryFs backing");
    backing_mounted = 1;
    snprintf(lower, sizeof(lower), "%s/lower", scratch);
    snprintf(upper, sizeof(upper), "%s/upper", scratch);
    snprintf(work, sizeof(work), "%s/work", scratch);
    snprintf(merged, sizeof(merged), "%s/merged", scratch);
    OVL_REQUIRE(mkdir(lower, 0755) == 0 && mkdir(upper, 0755) == 0 &&
                mkdir(work, 02777) == 0 && mkdir(merged, 0755) == 0,
                "create lower, upper, SGID work and merged roots");
    OVL_REQUIRE(chmod(work, 02777) == 0, "retain SGID on the public work root");
    snprintf(path, sizeof(path), "%s/deep", lower);
    OVL_REQUIRE(mkdir(path, 0755) == 0, "create lower-only ancestor");
    snprintf(source, sizeof(source), "%s/deep/seed", lower);
    fd = syscall(SYS_openat, AT_FDCWD, source, O_CREAT | O_EXCL | O_RDWR, 0755);
    OVL_REQUIRE(fd >= 0, "create native source");
    OVL_REQUIRE(syscall(SYS_write, fd, content, sizeof(content)) == (long)sizeof(content), "write source across native cache pages");
    OVL_REQUIRE(syscall(SYS_fchmod, fd, 06755) == 0, "initialize source setid permissions");
    OVL_REQUIRE(syscall(SYS_setxattr, source, "user.overlay", "retained", 8, 0) == 0, "initialize native source xattr");
    close(fd); fd = -1;
    snprintf(path, sizeof(path), "%s/orphan", lower);
    orphan = syscall(SYS_openat, AT_FDCWD, path, O_CREAT | O_EXCL | O_RDWR, 0600);
    OVL_REQUIRE(orphan >= 0 && syscall(SYS_write, orphan, "kept", 4) == 4, "create source for anonymous copy-up");
    close(orphan); orphan = -1;
    snprintf(options, sizeof(options), "lowerdir=%s,upperdir=%s,workdir=%s,index=off", lower, upper, work);
    OVL_REQUIRE(syscall(SYS_mount, "overlay", merged, "overlay", 0, options) == 0, "mount the merged filesystem");
    overlay_mounted = 1;
    snprintf(path, sizeof(path), "%s/deep/seed", merged);
    fd = syscall(SYS_openat, AT_FDCWD, path, O_RDONLY, 0);
    OVL_REQUIRE(fd >= 0, "retain lower-backed readonly fd");
    OVL_REQUIRE(syscall(SYS_fchmod, fd, 0600) == 0, "metadata copy-up prepares ancestor and data");
    OVL_REQUIRE(syscall(SYS_pread64, fd, observed, sizeof(observed), 0) == (long)sizeof(observed) &&
                memcmp(content, observed, sizeof(content)) == 0, "retained fd reads the complete copied content");
    OVL_REQUIRE(syscall(SYS_fchmod, fd, 06755) == 0 && syscall(SYS_fchown, fd, 77, 88) == 0,
                "forward chmod and chown as accepted intentions");
    OVL_REQUIRE(syscall(SYS_fstat, fd, &st) == 0 && st.st_uid == 77 && st.st_gid == 88 &&
                (st.st_mode & 07777) == 0755, "actual copied owner and chown setid removal");
    OVL_REQUIRE(stat(source, &st) == 0 && st.st_uid == 0 && (st.st_mode & 07777) == 06755,
                "copy-up preserves the original lower inode");
    snprintf(other, sizeof(other), "%s/deep/seed", upper);
    OVL_REQUIRE(syscall(SYS_getxattr, other, "user.overlay", observed, sizeof(observed)) == 8 &&
                memcmp(observed, "retained", 8) == 0, "actual upper preserves source xattr");
    OVL_REQUIRE(syscall(SYS_unlinkat, AT_FDCWD, path, 0) == 0, "unlink copied file and hide lower with whiteout");
    CHECK_ERR(syscall(SYS_openat, AT_FDCWD, path, O_RDONLY, 0), ENOENT, "whiteout hides the lower pathname");
    OVL_REQUIRE(syscall(SYS_fchmod, fd, 0640) == 0 && syscall(SYS_fstat, fd, &st) == 0 &&
                st.st_nlink == 0 && (st.st_mode & 07777) == 0640, "metadata still addresses the actual unlinked upper");
    close(fd); fd = -1;
    snprintf(path, sizeof(path), "%s/orphan", merged);
    orphan = syscall(SYS_openat, AT_FDCWD, path, O_RDONLY, 0);
    OVL_REQUIRE(orphan >= 0 && syscall(SYS_unlinkat, AT_FDCWD, path, 0) == 0,
                "unlink lower-only object while retaining its fd");
    CHECK_ERR(syscall(SYS_fchmod, orphan, 0640), EEXIST,
              "index=off rejects first copy-up of an already unlinked lower inode");
    OVL_REQUIRE(syscall(SYS_fstat, orphan, &st) == 0 && st.st_nlink == 1 &&
                (st.st_mode & 07777) == 0600, "rejected copy-up preserves the actual lower inode metadata");
    OVL_REQUIRE(syscall(SYS_pread64, orphan, observed, 4, 0) == 4 && memcmp(observed, "kept", 4) == 0,
                "retained lower inode still provides its original content");
    CHECK_ERR(syscall(SYS_openat, AT_FDCWD, path, O_RDONLY, 0), ENOENT, "rejected copy-up never recreates the old visible name");
    close(orphan); orphan = -1;
    snprintf(path, sizeof(path), "%s/new", merged);
    created = syscall(SYS_openat, AT_FDCWD, path, O_CREAT | O_EXCL | O_RDWR, 0640);
    OVL_REQUIRE(created >= 0 && syscall(SYS_write, created, "alias", 5) == 5, "create through the merged command owner");
    snprintf(other, sizeof(other), "%s/alias", merged);
    OVL_REQUIRE(syscall(SYS_linkat, AT_FDCWD, path, AT_FDCWD, other, 0) == 0 &&
                syscall(SYS_unlinkat, AT_FDCWD, path, 0) == 0, "link and unlink retain the shared native inode");
    OVL_REQUIRE(syscall(SYS_fstat, created, &st) == 0 && st.st_nlink == 1, "retained fd observes the surviving native link");
    snprintf(path, sizeof(path), "%s/renamed", merged);
    OVL_REQUIRE(syscall(SYS_renameat2, AT_FDCWD, other, AT_FDCWD, path, 0) == 0,
                "rename through the original operation context");
    snprintf(other, sizeof(other), "%s/through", merged);
    OVL_REQUIRE(syscall(SYS_symlinkat, "renamed", AT_FDCWD, other) == 0 &&
                syscall(SYS_readlinkat, AT_FDCWD, other, observed, sizeof(observed)) == 7 &&
                memcmp(observed, "renamed", 7) == 0, "create and read a merged symbolic link");
    OVL_REQUIRE(syscall(SYS_unlinkat, AT_FDCWD, other, 0) == 0 &&
                syscall(SYS_unlinkat, AT_FDCWD, path, 0) == 0, "remove both merged aliases");
    close(created); created = -1;
    snprintf(path, sizeof(path), "%s/directory", merged);
    snprintf(other, sizeof(other), "%s/moved-directory", merged);
    OVL_REQUIRE(syscall(SYS_mkdirat, AT_FDCWD, path, 0755) == 0 &&
                syscall(SYS_renameat2, AT_FDCWD, path, AT_FDCWD, other, 0) == 0 &&
                syscall(SYS_unlinkat, AT_FDCWD, other, AT_REMOVEDIR) == 0,
                "rename an upper directory and prune its private opaque marker on rmdir");
cleanup:
    if (fd >= 0) close(fd);
    if (orphan >= 0) close(orphan);
    if (created >= 0) close(created);
    if (overlay_mounted) CHECK_RET(syscall(SYS_umount2, merged, 0), 0, "unmount the merged view after closing its fds");
    if (backing_mounted) CHECK_RET(syscall(SYS_umount2, scratch, 0), 0, "unmount the isolated native backing");
    rmdir(scratch);
#undef OVL_REQUIRE
}

int main(int argc, char **argv) {
    TEST_START("vfs-ownership");
    if (argc == 2 && strcmp(argv[1], "--overlay-only") == 0) {
        overlay_ownership();
        TEST_DONE();
    }
    int null_fd = syscall(SYS_openat, AT_FDCWD, "/dev/null", O_CREAT | O_WRONLY, 0666);
    CHECK(null_fd >= 0, "O_CREAT opens an existing static device without modifying its parent");
    if (null_fd >= 0) close(null_fd);
    unix_ownership();
    if (argc == 2 && strcmp(argv[1], "--unix-only") == 0) {
        TEST_DONE();
    }
    overlay_ownership();

    char path_buf[256];
    int fd;
    uid_t test_uid = 70;
    gid_t test_gid = 70;

    /* ── Root-created files on tmpfs ─────────────────────── */
    const char *tmp_dir = "/tmp/vfs-owner-test-XXXXXX";
    char *dir_template = strdup(tmp_dir);
    if (!mkdtemp(dir_template)) {
        perror("mkdtemp");
        return 1;
    }
    CHECK_RET(chmod(dir_template, 0777), 0,
              "make tmpfs ownership test directory writable");

    snprintf(path_buf, sizeof(path_buf), "%s/root_file", dir_template);
    FILE *f = fopen(path_buf, "w");
    CHECK(f != NULL, "create file as root (tmpfs)");
    if (f) fclose(f);
    CHECK_RET(check_owner(path_buf, 0, 0), 0, "root file uid/gid (tmpfs)");

    snprintf(path_buf, sizeof(path_buf), "%s/root_dir", dir_template);
    CHECK_RET(mkdir(path_buf, 0755), 0, "create dir as root (tmpfs)");
    CHECK_RET(check_owner(path_buf, 0, 0), 0, "root dir uid/gid (tmpfs)");

    /* ── Root-created files on ext4 rootfs ───────────────── */
    const char *root_test_dir = "/vfs-owner-ext4-test";
    rmdir(root_test_dir);
    snprintf(path_buf, sizeof(path_buf), "%s/ext4_dir", root_test_dir);
    rmdir(path_buf);
    snprintf(path_buf, sizeof(path_buf), "%s/ext4_file", root_test_dir);
    unlink(path_buf);

    CHECK_RET(mkdir(root_test_dir, 0777), 0, "create ext4 test dir");
    CHECK_RET(chmod(root_test_dir, 0777), 0,
              "make ext4 ownership test directory writable");

    snprintf(path_buf, sizeof(path_buf), "%s/ext4_file", root_test_dir);
    fd = open(path_buf, O_CREAT | O_WRONLY, 0644);
    CHECK(fd >= 0, "create file as root (ext4)");
    if (fd >= 0) close(fd);
    CHECK_RET(check_owner(path_buf, 0, 0), 0, "root file uid/gid (ext4)");

    snprintf(path_buf, sizeof(path_buf), "%s/ext4_dir", root_test_dir);
    CHECK_RET(mkdir(path_buf, 0755), 0, "create dir as root (ext4)");
    CHECK_RET(check_owner(path_buf, 0, 0), 0, "root dir uid/gid (ext4)");

    /* ── Non-root (uid=70, gid=70) created files ────────── */
    CHECK_RET(switch_user(test_uid, test_gid), 0, "switch to uid=70");
    CHECK(geteuid() == test_uid, "effective uid is 70");
    CHECK(getegid() == test_gid, "effective gid is 70");

    /* tmpfs */
    snprintf(path_buf, sizeof(path_buf), "%s/postgres_file", dir_template);
    f = fopen(path_buf, "w");
    CHECK(f != NULL, "create file as uid 70 (tmpfs)");
    if (f) fclose(f);
    CHECK_RET(check_owner(path_buf, test_uid, test_gid), 0,
              "uid 70 file owner (tmpfs)");

    snprintf(path_buf, sizeof(path_buf), "%s/postgres_dir", dir_template);
    CHECK_RET(mkdir(path_buf, 0755), 0, "create dir as uid 70 (tmpfs)");
    CHECK_RET(check_owner(path_buf, test_uid, test_gid), 0,
              "uid 70 dir owner (tmpfs)");

    /* ext4 rootfs */
    snprintf(path_buf, sizeof(path_buf), "%s/ext4_noroot_file", root_test_dir);
    fd = open(path_buf, O_CREAT | O_WRONLY, 0644);
    CHECK(fd >= 0, "create file as uid 70 (ext4)");
    if (fd >= 0) close(fd);
    CHECK_RET(check_owner(path_buf, test_uid, test_gid), 0,
              "uid 70 file owner (ext4)");

    snprintf(path_buf, sizeof(path_buf), "%s/ext4_noroot_dir", root_test_dir);
    CHECK_RET(mkdir(path_buf, 0755), 0, "create dir as uid 70 (ext4)");
    CHECK_RET(check_owner(path_buf, test_uid, test_gid), 0,
              "uid 70 dir owner (ext4)");

    /* ── Ownership persists after switch back to root ───── */
    switch_user(0, 0);
    snprintf(path_buf, sizeof(path_buf), "%s/postgres_file", dir_template);
    CHECK_RET(check_owner(path_buf, test_uid, test_gid), 0,
              "persist: uid 70 tmpfs file still owned by 70");
    snprintf(path_buf, sizeof(path_buf), "%s/ext4_noroot_file", root_test_dir);
    CHECK_RET(check_owner(path_buf, test_uid, test_gid), 0,
              "persist: uid 70 ext4 file still owned by 70");

    /* ── chown by root ──────────────────────────────────── */
    /* ext4 file */
    snprintf(path_buf, sizeof(path_buf), "%s/ext4_file", root_test_dir);
    CHECK_RET(chown(path_buf, 100, 200), 0, "root chown ext4 file to 100:200");
    CHECK_RET(check_owner(path_buf, 100, 200), 0,
              "ext4 file ownership changed to 100:200");

    /* ext4 dir */
    snprintf(path_buf, sizeof(path_buf), "%s/ext4_dir", root_test_dir);
    CHECK_RET(chown(path_buf, 100, 200), 0, "root chown ext4 dir to 100:200");
    CHECK_RET(check_owner(path_buf, 100, 200), 0,
              "ext4 dir ownership changed to 100:200");

    /* tmpfs file */
    snprintf(path_buf, sizeof(path_buf), "%s/root_file", dir_template);
    CHECK_RET(chown(path_buf, 50, 60), 0, "root chown tmpfs file to 50:60");
    CHECK_RET(check_owner(path_buf, 50, 60), 0,
              "tmpfs file ownership changed to 50:60");

    /* chown back to root */
    snprintf(path_buf, sizeof(path_buf), "%s/ext4_file", root_test_dir);
    CHECK_RET(chown(path_buf, 0, 0), 0, "root chown ext4 file back to 0:0");
    CHECK_RET(check_owner(path_buf, 0, 0), 0,
              "ext4 file ownership restored to 0:0");

    /* ── fchown by root ─────────────────────────────────── */
    snprintf(path_buf, sizeof(path_buf), "%s/fchown_test", dir_template);
    fd = open(path_buf, O_CREAT | O_WRONLY, 0644);
    CHECK(fd >= 0, "create file for fchown test");
    CHECK_RET(fchown(fd, 77, 88), 0, "root fchown to 77:88");
    CHECK_RET(check_owner_fd(fd, 77, 88), 0, "fstat confirms fchown 77:88");
    close(fd);

    /* ── Non-root cannot chown another user's file ──────── */
    switch_user(test_uid, test_gid);
    snprintf(path_buf, sizeof(path_buf), "%s/root_file", dir_template);
    CHECK_ERR(chown(path_buf, 42, 42), EPERM,
              "uid 70 chown root-owned file → EPERM");
    CHECK_RET(check_owner(path_buf, 50, 60), 0,
              "root-owned file unchanged after denied chown");

    /* Non-root cannot give away own file */
    snprintf(path_buf, sizeof(path_buf), "%s/chown_self", dir_template);
    f = fopen(path_buf, "w");
    CHECK(f != NULL, "create self-owned file as uid 70");
    if (f) fclose(f);
    CHECK_ERR(chown(path_buf, 999, 999), EPERM,
              "uid 70 cannot give away file to uid 999");
    CHECK_RET(check_owner(path_buf, test_uid, test_gid), 0,
              "self-owned file unchanged after denied chown");

    /* ── Cleanup ────────────────────────────────────────── */
    switch_user(0, 0);
    /* tmpfs */
    snprintf(path_buf, sizeof(path_buf), "%s/postgres_file", dir_template);
    unlink(path_buf);
    snprintf(path_buf, sizeof(path_buf), "%s/root_file", dir_template);
    unlink(path_buf);
    snprintf(path_buf, sizeof(path_buf), "%s/chown_self", dir_template);
    unlink(path_buf);
    snprintf(path_buf, sizeof(path_buf), "%s/fchown_test", dir_template);
    unlink(path_buf);
    snprintf(path_buf, sizeof(path_buf), "%s/postgres_dir", dir_template);
    rmdir(path_buf);
    snprintf(path_buf, sizeof(path_buf), "%s/root_dir", dir_template);
    rmdir(path_buf);
    rmdir(dir_template);
    free(dir_template);
    /* ext4 */
    snprintf(path_buf, sizeof(path_buf), "%s/ext4_file", root_test_dir);
    unlink(path_buf);
    snprintf(path_buf, sizeof(path_buf), "%s/ext4_noroot_file", root_test_dir);
    unlink(path_buf);
    snprintf(path_buf, sizeof(path_buf), "%s/ext4_dir", root_test_dir);
    rmdir(path_buf);
    snprintf(path_buf, sizeof(path_buf), "%s/ext4_noroot_dir", root_test_dir);
    rmdir(path_buf);
    rmdir(root_test_dir);

    TEST_DONE();
    return 0;
}
