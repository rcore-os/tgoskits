#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include "test_framework.h"
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

/* Linux syscall numbers — macOS lacks setresuid/setresgid in libc */
#ifndef SYS_setresuid
#define SYS_setresuid 147
#endif
#ifndef SYS_setresgid
#define SYS_setresgid 170
#endif

static int switch_user(uid_t uid, gid_t gid) {
    if (syscall(SYS_setresgid, gid, gid, (gid_t)0) != 0) {
        fprintf(stderr, "setresgid(%u) failed: %s\n", gid, strerror(errno));
        return -1;
    }
    if (syscall(SYS_setresuid, uid, uid, (uid_t)0) != 0) {
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

int main(void) {
    TEST_START("vfs-ownership");

    /* ── Root-created files (tmpfs) ────────────────────────── */
    const char *tmp_dir = "/tmp/vfs-owner-test-XXXXXX";
    char *dir_template = strdup(tmp_dir);
    if (!mkdtemp(dir_template)) {
        perror("mkdtemp");
        return 1;
    }

    char path_buf[256];
    int fd;

    /* Create file as root on tmpfs */
    snprintf(path_buf, sizeof(path_buf), "%s/root_file", dir_template);
    FILE *f = fopen(path_buf, "w");
    CHECK(f != NULL, "create file as root (tmpfs)");
    if (f) fclose(f);
    CHECK_RET(check_owner(path_buf, 0, 0), 0, "root file uid/gid (tmpfs)");

    /* Create dir as root on tmpfs */
    snprintf(path_buf, sizeof(path_buf), "%s/root_dir", dir_template);
    CHECK_RET(mkdir(path_buf, 0755), 0, "create dir as root (tmpfs)");
    CHECK_RET(check_owner(path_buf, 0, 0), 0, "root dir uid/gid (tmpfs)");

    /* ── Root-created files (ext4 rootfs) ───────────────────── */
    const char *root_test_dir = "/root/vfs-owner-ext4-test";

    /* Clean up from previous run */
    rmdir(root_test_dir);
    snprintf(path_buf, sizeof(path_buf), "%s/ext4_dir", root_test_dir);
    rmdir(path_buf);
    snprintf(path_buf, sizeof(path_buf), "%s/ext4_file", root_test_dir);
    unlink(path_buf);

    CHECK_RET(mkdir(root_test_dir, 0755), 0, "create ext4 test dir");

    /* Create file as root on ext4 */
    snprintf(path_buf, sizeof(path_buf), "%s/ext4_file", root_test_dir);
    fd = open(path_buf, O_CREAT | O_WRONLY, 0644);
    CHECK(fd >= 0, "create file as root (ext4)");
    if (fd >= 0) close(fd);
    CHECK_RET(check_owner(path_buf, 0, 0), 0, "root file uid/gid (ext4)");

    /* Create dir as root on ext4 */
    snprintf(path_buf, sizeof(path_buf), "%s/ext4_dir", root_test_dir);
    CHECK_RET(mkdir(path_buf, 0755), 0, "create dir as root (ext4)");
    CHECK_RET(check_owner(path_buf, 0, 0), 0, "root dir uid/gid (ext4)");

    /* ── Non-root (uid=70, gid=70) created files ──────────── */
    uid_t test_uid = 70;
    gid_t test_gid = 70;

    CHECK_RET(switch_user(test_uid, test_gid), 0, "switch to uid=70");

    /* Verify we are now uid 70 */
    CHECK(geteuid() == test_uid, "effective uid is 70");
    CHECK(getegid() == test_gid, "effective gid is 70");

    /* Create file as uid 70 on tmpfs */
    snprintf(path_buf, sizeof(path_buf), "%s/postgres_file", dir_template);
    f = fopen(path_buf, "w");
    CHECK(f != NULL, "create file as uid 70 (tmpfs)");
    if (f) fclose(f);
    CHECK_RET(check_owner(path_buf, test_uid, test_gid), 0,
              "uid 70 file owner (tmpfs)");

    /* Create dir as uid 70 on tmpfs */
    snprintf(path_buf, sizeof(path_buf), "%s/postgres_dir", dir_template);
    CHECK_RET(mkdir(path_buf, 0755), 0, "create dir as uid 70 (tmpfs)");
    CHECK_RET(check_owner(path_buf, test_uid, test_gid), 0,
              "uid 70 dir owner (tmpfs)");

    /* Create file as uid 70 on ext4 */
    snprintf(path_buf, sizeof(path_buf), "%s/ext4_noroot_file", root_test_dir);
    fd = open(path_buf, O_CREAT | O_WRONLY, 0644);
    CHECK(fd >= 0, "create file as uid 70 (ext4)");
    if (fd >= 0) close(fd);
    CHECK_RET(check_owner(path_buf, test_uid, test_gid), 0,
              "uid 70 file owner (ext4)");

    /* Create dir as uid 70 on ext4 */
    snprintf(path_buf, sizeof(path_buf), "%s/ext4_noroot_dir", root_test_dir);
    CHECK_RET(mkdir(path_buf, 0755), 0, "create dir as uid 70 (ext4)");
    CHECK_RET(check_owner(path_buf, test_uid, test_gid), 0,
              "uid 70 dir owner (ext4)");

    /* ── Ownership persists after close and re-stat ────────── */
    /* Switch back to root to verify ownership persisted */
    switch_user(0, 0);
    snprintf(path_buf, sizeof(path_buf), "%s/postgres_file", dir_template);
    CHECK_RET(check_owner(path_buf, test_uid, test_gid), 0,
              "persist: uid 70 file still owned by 70 after switch back to root");
    snprintf(path_buf, sizeof(path_buf), "%s/ext4_noroot_file", root_test_dir);
    CHECK_RET(check_owner(path_buf, test_uid, test_gid), 0,
              "persist: uid 70 ext4 file still owned by 70 after switch back");

    /* ── chown behavior ────────────────────────────────────── */

    /* Root can chown any file */
    snprintf(path_buf, sizeof(path_buf), "%s/ext4_file", root_test_dir);
    CHECK_RET(chown(path_buf, 100, 200), 0, "root chown ext4 file to 100:200");
    CHECK_RET(check_owner(path_buf, 100, 200), 0,
              "ext4 file ownership changed to 100:200");

    /* Root can chown directories */
    snprintf(path_buf, sizeof(path_buf), "%s/ext4_dir", root_test_dir);
    CHECK_RET(chown(path_buf, 100, 200), 0, "root chown ext4 dir to 100:200");
    CHECK_RET(check_owner(path_buf, 100, 200), 0,
              "ext4 dir ownership changed to 100:200");

    /* Root can chown files on tmpfs */
    snprintf(path_buf, sizeof(path_buf), "%s/root_file", dir_template);
    CHECK_RET(chown(path_buf, 50, 60), 0, "root chown tmpfs file to 50:60");
    CHECK_RET(check_owner(path_buf, 50, 60), 0,
              "tmpfs file ownership changed to 50:60");

    /* Root can chown back to original owner */
    snprintf(path_buf, sizeof(path_buf), "%s/ext4_file", root_test_dir);
    CHECK_RET(chown(path_buf, 0, 0), 0, "root chown ext4 file back to 0:0");
    CHECK_RET(check_owner(path_buf, 0, 0), 0,
              "ext4 file ownership restored to 0:0");

    /* fchown via fd */
    snprintf(path_buf, sizeof(path_buf), "%s/fchown_test", dir_template);
    fd = open(path_buf, O_CREAT | O_WRONLY, 0644);
    CHECK(fd >= 0, "create file for fchown test");
    CHECK_RET(fchown(fd, 77, 88), 0, "root fchown to 77:88");
    CHECK_RET(check_owner_fd(fd, 77, 88), 0, "fstat confirms fchown 77:88");
    close(fd);

    /* Non-root cannot chown another user's file */
    snprintf(path_buf, sizeof(path_buf), "%s/root_file", dir_template);
    CHECK_ERR(chown(path_buf, 42, 42), EPERM,
              "uid 70 chown root-owned file → EPERM");
    /* Ownership unchanged after failed chown */
    CHECK_RET(check_owner(path_buf, 50, 60), 0,
              "root-owned tmpfs file still 50:60 after denied chown");

    /* Non-root cannot chown file to another user (even own file) */
    snprintf(path_buf, sizeof(path_buf), "%s/chown_self", dir_template);
    f = fopen(path_buf, "w");
    CHECK(f != NULL, "create self-owned file as uid 70");
    if (f) fclose(f);
    /* uid 70 tries to give their file to uid 999 — should fail */
    CHECK_ERR(chown(path_buf, 999, 999), EPERM,
              "uid 70 cannot give away file to uid 999");
    CHECK_RET(check_owner(path_buf, test_uid, test_gid), 0,
              "self-owned file unchanged after denied chown");

    /* ── Cleanup ─────────────────────────────────────────── */
    /* Remove tmpfs test files */
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

    /* Remove ext4 test files */
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
