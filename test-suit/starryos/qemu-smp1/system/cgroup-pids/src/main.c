/*
 * cgroup-pids — Verify cgroup v2 pids controller enforcement.
 *
 * Tests:
 *   1. Root cgroup pids files exist and are readable
 *   2. Root cgroup pids.max limits fork (should pass — code uses GLOBAL_CGROUP_ROOT)
 *   3. Child cgroup pids.max limits fork (TDD: expected to FAIL until
 *      per-process cgroup tracking is implemented)
 *   4. pids.current tracks process count correctly
 *   5. cpu controller stub files are readable
 */

#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static int __pass = 0;
static int __fail = 0;

#define CHECK(cond, msg) do {                                           \
    if (cond) {                                                         \
        printf("  PASS | %s:%d | %s\n", __FILE__, __LINE__, msg);       \
        __pass++;                                                       \
    } else {                                                            \
        printf("  FAIL | %s:%d | %s | errno=%d (%s)\n",                 \
               __FILE__, __LINE__, msg, errno, strerror(errno));        \
        __fail++;                                                       \
    }                                                                   \
} while (0)

#define TEST_START(name)                                                \
    printf("================================================\n");       \
    printf("  TEST: %s\n", name);                                       \
    printf("  FILE: %s\n", __FILE__);                                   \
    printf("================================================\n")

#define TEST_DONE()                                                     \
    printf("------------------------------------------------\n");       \
    printf("  DONE: %d pass, %d fail\n", __pass, __fail);               \
    printf("================================================\n\n");     \
    return __fail > 0 ? 1 : 0

#define CGROUP_ROOT "/cgroup"
#define CGROUP_CHILD CGROUP_ROOT "/tdd-pids"

/* ---- helpers ---- */

static ssize_t read_text(const char *path, char *buf, size_t cap)
{
    if (cap == 0) return -1;
    int fd = open(path, O_RDONLY);
    if (fd < 0) return -1;
    ssize_t n = read(fd, buf, cap - 1);
    if (n >= 0) buf[n] = '\0';
    close(fd);
    return n;
}

static int write_text(const char *path, const char *data)
{
    int fd = open(path, O_WRONLY);
    if (fd < 0) return -1;
    ssize_t n = write(fd, data, strlen(data));
    close(fd);
    return n >= 0 ? 0 : -1;
}

static int file_exists(const char *path)
{
    struct stat st;
    return stat(path, &st) == 0;
}

static void expect_write_ok(const char *path, const char *data, const char *msg)
{
    errno = 0;
    int ret = write_text(path, data);
    CHECK(ret == 0, msg);
}

/* expect_write_errno removed — not used in this test */

static int read_int(const char *path)
{
    char buf[32];
    if (read_text(path, buf, sizeof(buf)) < 0) return -1;
    return atoi(buf);
}

static int read_pids_current(const char *cgroup_path)
{
    char path[256];
    snprintf(path, sizeof(path), "%s/pids.current", cgroup_path);
    return read_int(path);
}

/* ---- fork helper: returns child pid or -1 ---- */

static pid_t try_fork(void)
{
    pid_t pid = fork();
    if (pid == 0) {
        /* child: sleep briefly then exit */
        usleep(50000);
        _exit(0);
    }
    return pid;
}

/* wait_for_all removed — using direct waitpid calls */

/* ================================================================ */

/*
 * Test 1: Root cgroup pids files exist and are readable.
 */
static void test_root_files(void)
{
    char buf[4096];
    ssize_t n;

    CHECK(file_exists(CGROUP_ROOT), "root cgroup mount exists");

    n = read_text(CGROUP_ROOT "/cgroup.controllers", buf, sizeof(buf));
    CHECK(n >= 0, "read root cgroup.controllers");
    if (n >= 0) {
        CHECK(strstr(buf, "pids") != NULL,
              "root cgroup.controllers lists pids");
        CHECK(strstr(buf, "cpu") != NULL,
              "root cgroup.controllers lists cpu");
        printf("  INFO | cgroup.controllers = %s", buf);
    }

    n = read_text(CGROUP_ROOT "/pids.max", buf, sizeof(buf));
    CHECK(n >= 0, "read root pids.max");
    if (n >= 0) {
        CHECK(strstr(buf, "max") != NULL,
              "root pids.max is \"max\" (unlimited) by default");
        printf("  INFO | pids.max = %s", buf);
    }

    n = read_text(CGROUP_ROOT "/pids.current", buf, sizeof(buf));
    CHECK(n >= 0, "read root pids.current");
    if (n >= 0) {
        int current = atoi(buf);
        CHECK(current > 0, "root pids.current > 0 (init process registered)");
        printf("  INFO | pids.current = %d\n", current);
    }
}

/*
 * Test 2: Root cgroup pids.max actually limits fork.
 *
 * This should PASS because clone.rs uses GLOBAL_CGROUP_ROOT.pids.can_fork().
 */
static void test_root_pids_limit(void)
{
    char path[256];
    char buf[64];

    /* Save original pids.current */
    int before = read_pids_current(CGROUP_ROOT);
    CHECK(before >= 0, "read root pids.current before test");

    /* Set pids.max = current + 1 (allow exactly one more process) */
    char limit[32];
    snprintf(limit, sizeof(limit), "%d", before + 1);
    snprintf(path, sizeof(path), "%s/pids.max", CGROUP_ROOT);
    expect_write_ok(path, limit, "write pids.max = current+1 on root");

    /* Verify the limit was set */
    ssize_t n = read_text(path, buf, sizeof(buf));
    CHECK(n >= 0, "read back pids.max");
    if (n >= 0) {
        CHECK(atoi(buf) == before + 1, "pids.max matches written value");
    }

    /* Fork one child — should succeed (we have 1 slot) */
    pid_t child1 = try_fork();
    CHECK(child1 > 0, "first fork succeeds (within pids limit)");

    /* Fork another child IMMEDIATELY — should fail with EAGAIN.
     * We must NOT wait for child1 to exit, because that would decrement
     * pids.current and free up a slot. */
    errno = 0;
    pid_t child2 = try_fork();
    if (child2 == 0) {
        /* We're the unexpected child — exit immediately */
        _exit(0);
    }
    if (child2 > 0) {
        /* Unexpected success — clean up and report */
        int status;
        waitpid(child2, &status, 0);
        CHECK(0, "second fork should fail with EAGAIN (but it succeeded)");
    } else {
        CHECK(errno == EAGAIN || errno == ENOMEM,
              "second fork fails with EAGAIN when pids limit reached");
    }

    /* Clean up: wait for child1, then restore pids.max */
    if (child1 > 0) {
        int status;
        waitpid(child1, &status, 0);
    }
    snprintf(path, sizeof(path), "%s/pids.max", CGROUP_ROOT);
    expect_write_ok(path, "max", "restore root pids.max to unlimited");
}

/*
 * Test 3: Child cgroup pids.max limits fork.
 *
 * TDD: This test documents the DESIRED behavior.
 * Current code always checks GLOBAL_CGROUP_ROOT, so child cgroup limits
 * are NOT enforced.  This test is expected to FAIL until per-process
 * cgroup tracking is implemented.
 */
static void test_child_pids_limit(void)
{
    char path[256];
    char buf[64];

    /* Enable +pids in root subtree_control so child cgroups
     * expose pids.max / pids.current files. */
    expect_write_ok(CGROUP_ROOT "/cgroup.subtree_control", "+pids",
                    "enable +pids in root subtree_control");

    /* Create child cgroup */
    errno = 0;
    int ret = mkdir(CGROUP_CHILD, 0755);
    CHECK(ret == 0 || errno == EEXIST, "mkdir child cgroup for pids test");

    /* Verify pids files exist on child */
    snprintf(path, sizeof(path), "%s/pids.max", CGROUP_CHILD);
    CHECK(file_exists(path), "child pids.max exists");

    snprintf(path, sizeof(path), "%s/pids.current", CGROUP_CHILD);
    CHECK(file_exists(path), "child pids.current exists");

    snprintf(path, sizeof(path), "%s/cgroup.controllers", CGROUP_CHILD);
    CHECK(file_exists(path), "child cgroup.controllers exists");

    /* Set child pids.max = 2 */
    snprintf(path, sizeof(path), "%s/pids.max", CGROUP_CHILD);
    expect_write_ok(path, "2", "write child pids.max = 2");

    /* Read back */
    ssize_t n = read_text(path, buf, sizeof(buf));
    CHECK(n >= 0, "read child pids.max");
    if (n >= 0) {
        CHECK(atoi(buf) == 2, "child pids.max reads back as 2");
    }

    /* Move current process to child cgroup */
    snprintf(path, sizeof(path), "%s/cgroup.procs", CGROUP_CHILD);
    char pid_str[32];
    snprintf(pid_str, sizeof(pid_str), "%d", getpid());
    expect_write_ok(path, pid_str, "move current process to child cgroup");

    /* Verify pids.current on child */
    snprintf(path, sizeof(path), "%s/pids.current", CGROUP_CHILD);
    int current = read_int(path);
    CHECK(current >= 1, "child pids.current >= 1 after migration");

    /* Try to fork — should succeed (within limit of 2) */
    pid_t child1 = try_fork();
    CHECK(child1 > 0, "first fork in child cgroup succeeds");

    /* Try to fork again IMMEDIATELY — should fail with EAGAIN (limit = 2,
     * already 2: parent + child1).  Do NOT wait for child1 first. */
    errno = 0;
    pid_t child2 = try_fork();
    if (child2 == 0) {
        _exit(0);
    }
    if (child2 > 0) {
        int status;
        waitpid(child2, &status, 0);
        CHECK(0,
              "TDD: second fork in child cgroup should fail (but succeeded) — "
              "child cgroup pids limit not enforced yet");
    } else {
        CHECK(errno == EAGAIN || errno == ENOMEM,
              "TDD: second fork in child cgroup fails with EAGAIN — "
              "per-process cgroup tracking works!");
    }

    /* Clean up children */
    if (child1 > 0) {
        int status;
        waitpid(child1, &status, 0);
    }

    /* Move back to root */
    snprintf(path, sizeof(path), "%s/cgroup.procs", CGROUP_ROOT);
    expect_write_ok(path, pid_str, "move current process back to root");

    /* Cleanup */
    rmdir(CGROUP_CHILD);
}

/*
 * Test 4: cpu controller stub files are readable.
 */
static void test_cpu_stub_files(void)
{
    char buf[256];
    ssize_t n;

    n = read_text(CGROUP_ROOT "/cpu.weight", buf, sizeof(buf));
    CHECK(n >= 0, "read root cpu.weight");
    if (n >= 0) {
        CHECK(atoi(buf) == 100, "root cpu.weight default is 100");
        printf("  INFO | cpu.weight = %s", buf);
    }

    n = read_text(CGROUP_ROOT "/cpu.max", buf, sizeof(buf));
    CHECK(n >= 0, "read root cpu.max");
    if (n >= 0) {
        CHECK(strstr(buf, "max") != NULL,
              "root cpu.max is \"max\" (unlimited) by default");
        printf("  INFO | cpu.max = %s", buf);
    }

    n = read_text(CGROUP_ROOT "/cpu.stat", buf, sizeof(buf));
    CHECK(n >= 0, "read root cpu.stat");
    if (n >= 0) {
        CHECK(strstr(buf, "nr_periods") != NULL,
              "root cpu.stat contains nr_periods");
        printf("  INFO | cpu.stat = %s", buf);
    }

    /* Write cpu.weight — should succeed (even though no enforcement) */
    expect_write_ok(CGROUP_ROOT "/cpu.weight", "200",
                    "write cpu.weight = 200");
    n = read_text(CGROUP_ROOT "/cpu.weight", buf, sizeof(buf));
    CHECK(n >= 0 && atoi(buf) == 200, "cpu.weight reads back as 200");

    /* Restore default */
    expect_write_ok(CGROUP_ROOT "/cpu.weight", "100",
                    "restore cpu.weight = 100");

    /* Write cpu.max — should succeed */
    expect_write_ok(CGROUP_ROOT "/cpu.max", "50000 100000",
                    "write cpu.max = 50000 100000");
    n = read_text(CGROUP_ROOT "/cpu.max", buf, sizeof(buf));
    CHECK(n >= 0, "read back cpu.max");
    if (n >= 0) {
        CHECK(strstr(buf, "50000") != NULL, "cpu.max contains 50000");
    }

    /* Restore default */
    expect_write_ok(CGROUP_ROOT "/cpu.max", "max 100000",
                    "restore cpu.max = max 100000");
}

/*
 * Test 4: pids.max = "max" (unlimited) — fork should always succeed.
 */
static void test_pids_unlimited(void)
{
    char path[256];

    /* Ensure pids.max is "max" */
    snprintf(path, sizeof(path), "%s/pids.max", CGROUP_ROOT);
    expect_write_ok(path, "max", "set pids.max = max (unlimited)");

    /* Fork multiple children — all should succeed */
    pid_t children[4];
    int count = 0;
    for (int i = 0; i < 4; i++) {
        children[i] = fork();
        if (children[i] == 0) {
            usleep(100000);
            _exit(0);
        }
        if (children[i] > 0) count++;
    }

    CHECK(count == 4, "all 4 forks succeed with pids.max = max");

    /* Wait for all children */
    for (int i = 0; i < 4; i++) {
        if (children[i] > 0) {
            int status;
            waitpid(children[i], &status, 0);
        }
    }
}

/*
 * Test 5: pids.current decrements when children exit.
 */
static void test_pids_current_decrement(void)
{
    /* Record initial pids.current */
    int before = read_pids_current(CGROUP_ROOT);
    CHECK(before >= 0, "read pids.current before fork");

    /* Fork a child */
    pid_t child = fork();
    if (child == 0) {
        _exit(0);
    }
    CHECK(child > 0, "fork child for decrement test");

    /* Wait for child to exit */
    int status;
    waitpid(child, &status, 0);

    /* Give the kernel time to update pids.current */
    usleep(10000);

    /* pids.current should be back to the original value */
    int after = read_pids_current(CGROUP_ROOT);
    CHECK(after == before,
          "pids.current returns to original after child exits");
}

/*
 * Test 6: cgroup.procs read-back contains current PID.
 */
static void test_cgroup_procs_readback(void)
{
    char path[256];
    char buf[4096];

    /* Move to root cgroup */
    snprintf(path, sizeof(path), "%s/cgroup.procs", CGROUP_ROOT);
    char pid_str[32];
    snprintf(pid_str, sizeof(pid_str), "%d", getpid());
    expect_write_ok(path, pid_str, "write current PID to root cgroup.procs");

    /* Read back and check */
    ssize_t n = read_text(path, buf, sizeof(buf));
    CHECK(n >= 0, "read root cgroup.procs");
    if (n >= 0) {
        char *found = strstr(buf, pid_str);
        CHECK(found != NULL,
              "root cgroup.procs contains current PID");
    }
}

/*
 * Test 7: pids.max = 0 — no new processes allowed.
 */
static void test_pids_max_zero(void)
{
    char path[256];

    /* Set pids.max = 0 */
    snprintf(path, sizeof(path), "%s/pids.max", CGROUP_ROOT);
    expect_write_ok(path, "0", "set pids.max = 0");

    /* Fork should fail */
    errno = 0;
    pid_t child = fork();
    if (child == 0) {
        _exit(0);
    }
    if (child > 0) {
        int status;
        waitpid(child, &status, 0);
        CHECK(0, "fork should fail with pids.max = 0 (but succeeded)");
    } else {
        CHECK(errno == EAGAIN || errno == ENOMEM,
              "fork fails with EAGAIN when pids.max = 0");
    }

    /* Restore */
    expect_write_ok(path, "max", "restore pids.max = max");
}

/*
 * Test 8: Migration between cgroups updates pids.current.
 */
static void test_migration_updates_count(void)
{
    char path[256];
    char pid_str[32];
    snprintf(pid_str, sizeof(pid_str), "%d", getpid());

    /* Enable +pids in root subtree_control so child cgroups
     * expose pids.current file. */
    expect_write_ok(CGROUP_ROOT "/cgroup.subtree_control", "+pids",
                    "enable +pids in root subtree_control for migration test");

    /* Create a child cgroup */
    errno = 0;
    mkdir(CGROUP_CHILD, 0755);

    /* Record root pids.current before migration */
    int root_before = read_pids_current(CGROUP_ROOT);

    /* Move to child cgroup */
    snprintf(path, sizeof(path), "%s/cgroup.procs", CGROUP_CHILD);
    expect_write_ok(path, pid_str, "move to child cgroup");

    /* Give kernel time to update */
    usleep(10000);

    /* Child cgroup should have pids.current >= 1 */
    int child_current = read_pids_current(CGROUP_CHILD);
    CHECK(child_current >= 1,
          "child cgroup pids.current >= 1 after migration");

    /* Root cgroup pids.current should have decreased */
    int root_after = read_pids_current(CGROUP_ROOT);
    CHECK(root_after < root_before,
          "root pids.current decreased after migration");

    /* Move back to root */
    snprintf(path, sizeof(path), "%s/cgroup.procs", CGROUP_ROOT);
    expect_write_ok(path, pid_str, "move back to root cgroup");

    /* Cleanup */
    rmdir(CGROUP_CHILD);
}

/*
 * Test 9: Nested cgroup pids isolation.
 */
static void test_nested_cgroup_isolation(void)
{
    char path[256];
    char parent_path[256], child_path[256];

    /* Enable +pids in root subtree_control so child cgroups
     * expose pids.max file. */
    expect_write_ok(CGROUP_ROOT "/cgroup.subtree_control", "+pids",
                    "enable +pids in root subtree_control for nested test");

    /* Create parent and child cgroups */
    snprintf(parent_path, sizeof(parent_path), "%s/nest-parent", CGROUP_ROOT);
    snprintf(child_path, sizeof(child_path), "%s/nest-parent/child", CGROUP_ROOT);

    errno = 0;
    mkdir(parent_path, 0755);
    mkdir(child_path, 0755);

    /* Set different limits */
    snprintf(path, sizeof(path), "%s/pids.max", parent_path);
    expect_write_ok(path, "10", "set parent pids.max = 10");

    snprintf(path, sizeof(path), "%s/pids.max", child_path);
    expect_write_ok(path, "3", "set child pids.max = 3");

    /* Verify independence */
    snprintf(path, sizeof(path), "%s/pids.max", parent_path);
    char buf[32];
    read_text(path, buf, sizeof(buf));
    CHECK(atoi(buf) == 10, "parent pids.max is 10");

    snprintf(path, sizeof(path), "%s/pids.max", child_path);
    read_text(path, buf, sizeof(buf));
    CHECK(atoi(buf) == 3, "child pids.max is 3");

    /* Cleanup */
    rmdir(child_path);
    rmdir(parent_path);
}

/* ================================================================ */

int main(void)
{
    TEST_START("cgroup-pids");

    /* Mount cgroup2 at CGROUP_ROOT if not already mounted */
    mkdir(CGROUP_ROOT, 0755);
    errno = 0;
    int ret = mount("none", CGROUP_ROOT, "cgroup2", 0, NULL);
    if (ret != 0 && errno != EBUSY) {
        printf("  FAIL | mount cgroup2 at %s failed: %s\n",
               CGROUP_ROOT, strerror(errno));
        TEST_DONE();
    }

    test_root_files();
    test_root_pids_limit();
    test_child_pids_limit();
    test_pids_unlimited();
    test_pids_current_decrement();
    test_cgroup_procs_readback();
    test_pids_max_zero();
    test_migration_updates_count();
    test_nested_cgroup_isolation();
    test_cpu_stub_files();

    TEST_DONE();
}
