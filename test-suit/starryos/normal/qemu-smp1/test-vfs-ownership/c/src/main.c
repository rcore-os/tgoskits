#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include "test_framework.h"
#include <errno.h>
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

int main(void) {
    TEST_START("vfs-ownership");

    /* ── Root-created files ──────────────────────────────── */
    const char *tmp_dir = "/tmp/vfs-owner-test-XXXXXX";
    char *dir_template = strdup(tmp_dir);
    if (!mkdtemp(dir_template)) {
        perror("mkdtemp");
        return 1;
    }

    char path_buf[256];

    /* Create file as root */
    snprintf(path_buf, sizeof(path_buf), "%s/root_file", dir_template);
    FILE *f = fopen(path_buf, "w");
    CHECK(f != NULL, "create file as root");
    if (f) fclose(f);
    CHECK_RET(check_owner(path_buf, 0, 0), 0, "root file uid/gid");

    /* Create dir as root */
    snprintf(path_buf, sizeof(path_buf), "%s/root_dir", dir_template);
    CHECK_RET(mkdir(path_buf, 0755), 0, "create dir as root");
    CHECK_RET(check_owner(path_buf, 0, 0), 0, "root dir uid/gid");

    /* ── Non-root (uid=70, gid=70) created files ──────────── */
    uid_t test_uid = 70;
    gid_t test_gid = 70;

    CHECK_RET(switch_user(test_uid, test_gid), 0, "switch to uid=70");

    /* Verify we are now uid 70 */
    CHECK(geteuid() == test_uid, "effective uid is 70");
    CHECK(getegid() == test_gid, "effective gid is 70");

    /* Create file as uid 70 */
    snprintf(path_buf, sizeof(path_buf), "%s/postgres_file", dir_template);
    f = fopen(path_buf, "w");
    CHECK(f != NULL, "create file as uid 70");
    if (f) fclose(f);
    CHECK_RET(check_owner(path_buf, test_uid, test_gid), 0,
              "uid 70 file owner");

    /* Create dir as uid 70 */
    snprintf(path_buf, sizeof(path_buf), "%s/postgres_dir", dir_template);
    CHECK_RET(mkdir(path_buf, 0755), 0, "create dir as uid 70");
    CHECK_RET(check_owner(path_buf, test_uid, test_gid), 0,
              "uid 70 dir owner");

    /* ── Cleanup ─────────────────────────────────────────── */
    switch_user(0, 0);
    snprintf(path_buf, sizeof(path_buf), "%s/postgres_file", dir_template);
    unlink(path_buf);
    snprintf(path_buf, sizeof(path_buf), "%s/root_file", dir_template);
    unlink(path_buf);
    snprintf(path_buf, sizeof(path_buf), "%s/postgres_dir", dir_template);
    rmdir(path_buf);
    snprintf(path_buf, sizeof(path_buf), "%s/root_dir", dir_template);
    rmdir(path_buf);
    rmdir(dir_template);
    free(dir_template);

    TEST_DONE();
    return 0;
}
