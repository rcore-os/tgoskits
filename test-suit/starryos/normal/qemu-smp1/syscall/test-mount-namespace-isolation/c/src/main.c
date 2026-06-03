#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <stdio.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static const char *BASE = "/tmp/mnt-ns-test";
static const char *SOURCE = "/tmp/mnt-ns-test/source";
static const char *TARGET = "/tmp/mnt-ns-test/target";
static const char *SOURCE_MARKER = "/tmp/mnt-ns-test/source/child-only";
static const char *TARGET_MARKER = "/tmp/mnt-ns-test/target/child-only";

static void write_marker(void)
{
    int fd = open(SOURCE_MARKER, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    CHECK(fd >= 0, "create source marker");
    if (fd < 0) {
        return;
    }

    const char *payload = "mount namespace marker\n";
    ssize_t written = write(fd, payload, strlen(payload));
    CHECK(written == (ssize_t)strlen(payload), "write source marker");
    CHECK_RET(close(fd), 0, "close source marker");
}

static void prepare_tree(void)
{
    mkdir(BASE, 0755);
    mkdir(SOURCE, 0755);
    mkdir(TARGET, 0755);
    CHECK_RET(access(SOURCE, F_OK), 0, "source directory exists");
    CHECK_RET(access(TARGET, F_OK), 0, "target directory exists");
    write_marker();
    CHECK_ERR(access(TARGET_MARKER, F_OK), ENOENT,
              "parent target starts without marker");
}

static void child_body(void)
{
    CHECK_RET(unshare(CLONE_NEWNS), 0, "child unshare(CLONE_NEWNS)");
    CHECK_RET(mount(SOURCE, TARGET, NULL, MS_BIND, NULL), 0,
              "child bind mount source onto target");
    CHECK_RET(access(TARGET_MARKER, F_OK), 0,
              "child sees marker through namespace-local mount");
    CHECK_RET(umount2(TARGET, MNT_DETACH), 0, "child detaches bind mount");
    _exit(__fail > 0 ? 1 : 0);
}

static void run_mount_namespace_isolation(void)
{
    prepare_tree();

    pid_t child = fork();
    CHECK(child >= 0, "fork child for mount namespace isolation");
    if (child == 0) {
        child_body();
    }

    int status = 0;
    CHECK_RET(waitpid(child, &status, 0), child, "wait for child");
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "child mount namespace checks passed");
    CHECK_ERR(access(TARGET_MARKER, F_OK), ENOENT,
              "parent does not see child bind mount marker");
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    TEST_START("mount namespace bind mount isolation");

    run_mount_namespace_isolation();

    TEST_DONE();
}
