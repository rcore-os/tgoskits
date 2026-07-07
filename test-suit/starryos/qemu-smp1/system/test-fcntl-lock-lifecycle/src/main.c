/*
 * Focused StarryOS regression test for advisory file lock lifecycle.
 *
 * Covers four scenarios from plan.md L3:
 *   1. child F_SETLK + close fd → parent F_SETLKW wakes
 *   2. child F_SETLK + exit → parent F_SETLKW wakes
 *   3. child FD_CLOEXEC + exec → parent F_SETLKW wakes
 *   4. OFD F_OFD_SETLKW last-close → waiter wakes
 *
 * Each scenario prints a unique pass marker.
 * Final marker: FCNTL_LOCK_LIFECYCLE_ALL_PASSED
 */
#define _GNU_SOURCE
#include "../common/test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

#define LOCK_FILE "/tmp/fcntl-lock-test"

static int create_lock_file(void)
{
    int fd = open(LOCK_FILE, O_CREAT | O_RDWR | O_TRUNC, 0644);
    CHECK(fd >= 0, "create lock test file");
    if (fd >= 0) {
        (void)write(fd, "locktest\n", 9);
    }
    return fd;
}

static void set_write_lock(int fd, int cmd, int *out_rc)
{
    struct flock fl;
    memset(&fl, 0, sizeof(fl));
    fl.l_type = F_WRLCK;
    fl.l_whence = SEEK_SET;
    fl.l_start = 0;
    fl.l_len = 0;
    *out_rc = fcntl(fd, cmd, &fl);
}

/* ── Scenario 1: child F_SETLK + close fd → parent F_SETLKW wakes ── */
static void test_child_lock_close_parent_wakes(void)
{
    TEST_START("child F_SETLK + close fd wakes parent F_SETLKW");

    int fd = create_lock_file();
    if (fd < 0) return;

    pid_t child = fork();
    CHECK(child >= 0, "fork for lock/close scenario");
    if (child == 0) {
        int rc;
        set_write_lock(fd, F_SETLK, &rc);
        CHECK(rc == 0, "child acquires lock");
        close(fd);
        _exit(0);
    }

    /* parent: wait for child to acquire lock, then try F_SETLKW */
    usleep(100000); /* let child lock */

    int rc;
    set_write_lock(fd, F_SETLKW, &rc);
    CHECK(rc == 0, "parent acquires lock after child close");

    /* reap child */
    int status;
    waitpid(child, &status, 0);

    close(fd);
    unlink(LOCK_FILE);
    printf("FCNTL_LOCK_CLOSE_WAKE_PASSED\n");
}

/* ── Scenario 2: child F_SETLK + exit → parent F_SETLKW wakes ── */
static void test_child_lock_exit_parent_wakes(void)
{
    TEST_START("child F_SETLK + exit wakes parent F_SETLKW");

    int fd = create_lock_file();
    if (fd < 0) return;

    pid_t child = fork();
    CHECK(child >= 0, "fork for lock/exit scenario");
    if (child == 0) {
        int rc;
        set_write_lock(fd, F_SETLK, &rc);
        CHECK(rc == 0, "child acquires lock before exit");
        /* fd is NOT closed — exit should release the lock */
        _exit(0);
    }

    usleep(100000);

    int rc;
    set_write_lock(fd, F_SETLKW, &rc);
    CHECK(rc == 0, "parent acquires lock after child exit");

    int status;
    waitpid(child, &status, 0);

    close(fd);
    unlink(LOCK_FILE);
    printf("FCNTL_LOCK_EXIT_WAKE_PASSED\n");
}

/* ── Scenario 3: child FD_CLOEXEC + exec → parent F_SETLKW wakes ── */
static void test_child_cloexec_exec_parent_wakes(void)
{
    TEST_START("child FD_CLOEXEC + exec wakes parent F_SETLKW");

    int fd = create_lock_file();
    if (fd < 0) return;

    /* set CLOEXEC on fd */
    int flags = fcntl(fd, F_GETFD);
    fcntl(fd, F_SETFD, flags | FD_CLOEXEC);

    pid_t child = fork();
    CHECK(child >= 0, "fork for cloexec/exec scenario");
    if (child == 0) {
        int rc;
        set_write_lock(fd, F_SETLK, &rc);
        CHECK(rc == 0, "child acquires lock before exec");

        /* exec /bin/true — fd with CLOEXEC should be closed */
        execl("/bin/true", "true", NULL);
        _exit(127);
    }

    usleep(100000);

    int rc;
    set_write_lock(fd, F_SETLKW, &rc);
    CHECK(rc == 0, "parent acquires lock after child exec+cloexec close");

    int status;
    waitpid(child, &status, 0);

    close(fd);
    unlink(LOCK_FILE);
    printf("FCNTL_LOCK_CLOEXEC_EXEC_WAKE_PASSED\n");
}

/* ── Scenario 4: OFD lock last-close → waiter wakes ── */
static void test_ofd_lock_last_close_wakes(void)
{
    TEST_START("OFD lock last-close wakes waiter");

    int fd = create_lock_file();
    if (fd < 0) return;

    pid_t child = fork();
    CHECK(child >= 0, "fork for OFD lock scenario");
    if (child == 0) {
        struct flock fl;
        memset(&fl, 0, sizeof(fl));
        fl.l_type = F_WRLCK;
        fl.l_whence = SEEK_SET;
        fl.l_start = 0;
        fl.l_len = 0;
        int rc = fcntl(fd, F_OFD_SETLK, &fl);
        CHECK(rc == 0, "child acquires OFD lock");
        close(fd);
        _exit(0);
    }

    usleep(100000);

    struct flock fl;
    memset(&fl, 0, sizeof(fl));
    fl.l_type = F_WRLCK;
    fl.l_whence = SEEK_SET;
    fl.l_start = 0;
    fl.l_len = 0;
    int rc = fcntl(fd, F_OFD_SETLKW, &fl);
    CHECK(rc == 0, "parent acquires OFD lock after child close");

    int status;
    waitpid(child, &status, 0);

    close(fd);
    unlink(LOCK_FILE);
    printf("FCNTL_OFD_LOCK_LAST_CLOSE_WAKE_PASSED\n");
}

int main(void)
{
    test_child_lock_close_parent_wakes();
    test_child_lock_exit_parent_wakes();
    test_child_cloexec_exec_parent_wakes();
    test_ofd_lock_last_close_wakes();

    printf("FCNTL_LOCK_LIFECYCLE_ALL_PASSED\n");
    return 0;
}
