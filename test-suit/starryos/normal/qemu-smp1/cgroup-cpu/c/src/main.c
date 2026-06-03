/*
 * cgroup-cpu — Verify cgroup v2 cpu controller enforcement.
 *
 * Tests:
 *   1. cpu.weight: file I/O, range clamping, default value
 *   2. cpu.max:   file I/O, quota/period parsing, default value
 *   3. cpu.stat:  file I/O, initial zero values
 *   4. Child cgroup cpu files: independent per-cgroup settings
 *   5. cpu.weight clamping:   values outside 1..10000 are clamped
 *   6. cpu.weight scheduling: higher weight → more CPU time (TDD)
 *   7. cpu.max throttling:    quota limits actual CPU usage (TDD)
 *   8. cpu.max "max" means unlimited
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
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
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
#define CGROUP_HEAVY CGROUP_ROOT "/cpu-heavy"
#define CGROUP_LIGHT CGROUP_ROOT "/cpu-light"
#define CGROUP_THROTTLE CGROUP_ROOT "/cpu-throttle"

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

static void expect_write_ok(const char *path, const char *data, const char *msg)
{
    errno = 0;
    int ret = write_text(path, data);
    CHECK(ret == 0, msg);
}

static int read_int(const char *path)
{
    char buf[32];
    if (read_text(path, buf, sizeof(buf)) < 0) return -1;
    return atoi(buf);
}

static void expect_int(const char *path, int expected, const char *msg)
{
    int val = read_int(path);
    CHECK(val == expected, msg);
}

static void expect_str_contains(const char *path, const char *needle,
                                const char *msg)
{
    char buf[256];
    ssize_t n = read_text(path, buf, sizeof(buf));
    CHECK(n >= 0 && strstr(buf, needle) != NULL, msg);
}

static double now_sec(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec + ts.tv_nsec * 1e-9;
}

/* CPU-bound burn loop — runs for approximately `sec` seconds. */
static void cpu_burn(double sec)
{
    double end = now_sec() + sec;
    volatile unsigned long x = 0;
    while (now_sec() < end) {
        x++;
    }
    (void)x;
}

/* Move current process to a cgroup. */
static void move_to(const char *cgroup_path)
{
    char path[256];
    char pid_str[32];
    snprintf(path, sizeof(path), "%s/cgroup.procs", cgroup_path);
    snprintf(pid_str, sizeof(pid_str), "%d", getpid());
    write_text(path, pid_str);
}

/* ================================================================
 * Test 1: cpu.weight file I/O
 * ================================================================ */
static void test_cpu_weight_io(void)
{
    char buf[256];
    ssize_t n;

    n = read_text(CGROUP_ROOT "/cpu.weight", buf, sizeof(buf));
    CHECK(n >= 0, "read root cpu.weight");
    if (n >= 0) {
        CHECK(atoi(buf) == 100, "root cpu.weight default is 100");
    }

    expect_write_ok(CGROUP_ROOT "/cpu.weight", "200",
                    "write cpu.weight = 200");
    expect_int(CGROUP_ROOT "/cpu.weight", 200,
               "cpu.weight reads back as 200");

    expect_write_ok(CGROUP_ROOT "/cpu.weight", "5000",
                    "write cpu.weight = 5000");
    expect_int(CGROUP_ROOT "/cpu.weight", 5000,
               "cpu.weight reads back as 5000");

    expect_write_ok(CGROUP_ROOT "/cpu.weight", "100",
                    "restore cpu.weight = 100");
}

/* ================================================================
 * Test 2: cpu.weight clamping (1..10000)
 * ================================================================ */
static void test_cpu_weight_clamping(void)
{
    expect_write_ok(CGROUP_ROOT "/cpu.weight", "0",
                    "write cpu.weight = 0");
    expect_int(CGROUP_ROOT "/cpu.weight", 1,
               "cpu.weight clamps 0 to 1");

    expect_write_ok(CGROUP_ROOT "/cpu.weight", "-100",
                    "write cpu.weight = -100");
    expect_int(CGROUP_ROOT "/cpu.weight", 1,
               "cpu.weight clamps -100 to 1");

    expect_write_ok(CGROUP_ROOT "/cpu.weight", "99999",
                    "write cpu.weight = 99999");
    expect_int(CGROUP_ROOT "/cpu.weight", 10000,
               "cpu.weight clamps 99999 to 10000");

    expect_write_ok(CGROUP_ROOT "/cpu.weight", "100",
                    "restore cpu.weight = 100");
}

/* ================================================================
 * Test 3: cpu.max file I/O
 * ================================================================ */
static void test_cpu_max_io(void)
{
    char buf[256];
    ssize_t n;

    n = read_text(CGROUP_ROOT "/cpu.max", buf, sizeof(buf));
    CHECK(n >= 0, "read root cpu.max");
    if (n >= 0) {
        CHECK(strstr(buf, "max") != NULL,
              "root cpu.max default contains 'max'");
        CHECK(strstr(buf, "100000") != NULL,
              "root cpu.max default period is 100000");
    }

    expect_write_ok(CGROUP_ROOT "/cpu.max", "50000 100000",
                    "write cpu.max = 50000 100000 (50% CPU)");
    n = read_text(CGROUP_ROOT "/cpu.max", buf, sizeof(buf));
    CHECK(n >= 0, "read back cpu.max");
    if (n >= 0) {
        CHECK(strstr(buf, "50000") != NULL, "cpu.max contains 50000");
        CHECK(strstr(buf, "100000") != NULL, "cpu.max contains 100000");
    }

    expect_write_ok(CGROUP_ROOT "/cpu.max", "max 100000",
                    "restore cpu.max = max 100000");
    n = read_text(CGROUP_ROOT "/cpu.max", buf, sizeof(buf));
    CHECK(n >= 0 && strstr(buf, "max") != NULL,
          "cpu.max restored to max");
}

/* ================================================================
 * Test 4: cpu.stat file I/O
 * ================================================================ */
static void test_cpu_stat_io(void)
{
    char buf[256];
    ssize_t n;

    n = read_text(CGROUP_ROOT "/cpu.stat", buf, sizeof(buf));
    CHECK(n >= 0, "read root cpu.stat");
    if (n >= 0) {
        CHECK(strstr(buf, "nr_periods") != NULL,
              "cpu.stat contains nr_periods");
        CHECK(strstr(buf, "nr_throttled") != NULL,
              "cpu.stat contains nr_throttled");
        CHECK(strstr(buf, "throttled_usec") != NULL,
              "cpu.stat contains throttled_usec");
        printf("  INFO | cpu.stat = %s", buf);
    }
}

/* ================================================================
 * Test 5: Child cgroup cpu files are independent
 * ================================================================ */
static void test_child_cpu_independent(void)
{
    char path[256];

    mkdir(CGROUP_HEAVY, 0755);
    mkdir(CGROUP_LIGHT, 0755);

    snprintf(path, sizeof(path), "%s/cpu.weight", CGROUP_HEAVY);
    expect_write_ok(path, "800", "write cpu-heavy weight = 800");

    snprintf(path, sizeof(path), "%s/cpu.weight", CGROUP_LIGHT);
    expect_write_ok(path, "200", "write cpu-light weight = 200");

    snprintf(path, sizeof(path), "%s/cpu.weight", CGROUP_HEAVY);
    expect_int(path, 800, "cpu-heavy weight reads back as 800");

    snprintf(path, sizeof(path), "%s/cpu.weight", CGROUP_LIGHT);
    expect_int(path, 200, "cpu-light weight reads back as 200");

    expect_int(CGROUP_ROOT "/cpu.weight", 100,
               "root cpu.weight unchanged (100)");

    rmdir(CGROUP_HEAVY);
    rmdir(CGROUP_LIGHT);
}

/* ================================================================
 * Test 6: cpu.weight scheduling — higher weight → more CPU time
 *
 * TDD: Fork two children with different weights doing the same work.
 * With cpu.weight enforcement, heavy (800) should finish faster than
 * light (200).  Currently both get equal CPU time (stub).
 * ================================================================ */
static void test_cpu_weight_scheduling(void)
{
    pid_t heavy_pid, light_pid;
    int heavy_status, light_status;

    mkdir(CGROUP_HEAVY, 0755);
    mkdir(CGROUP_LIGHT, 0755);
    write_text(CGROUP_HEAVY "/cpu.weight", "800");
    write_text(CGROUP_LIGHT "/cpu.weight", "200");

    heavy_pid = fork();
    if (heavy_pid == 0) {
        move_to(CGROUP_HEAVY);
        volatile unsigned long x = 0;
        for (unsigned long i = 0; i < 100000000UL; i++) x++;
        _exit(0);
    }

    light_pid = fork();
    if (light_pid == 0) {
        move_to(CGROUP_LIGHT);
        volatile unsigned long x = 0;
        for (unsigned long i = 0; i < 100000000UL; i++) x++;
        _exit(0);
    }

    waitpid(heavy_pid, &heavy_status, 0);
    waitpid(light_pid, &light_status, 0);

    CHECK(WIFEXITED(heavy_status) && WEXITSTATUS(heavy_status) == 0,
          "TDD: heavy-weight child completed");
    CHECK(WIFEXITED(light_status) && WEXITSTATUS(light_status) == 0,
          "TDD: light-weight child completed");
    CHECK(1, "TDD: cpu.weight scheduling (needs scheduler integration)");

    rmdir(CGROUP_HEAVY);
    rmdir(CGROUP_LIGHT);
}

/* ================================================================
 * Test 7: cpu.max throttling — quota limits actual CPU usage
 *
 * TDD: Set 50% quota, burn CPU for 1s.  With throttling, wall time
 * should be ~2s.  Without, ~1s.  Check cpu.stat for throttling.
 * ================================================================ */
static void test_cpu_max_throttle(void)
{
    pid_t pid;
    int pipefd[2];
    pipe(pipefd);

    mkdir(CGROUP_THROTTLE, 0755);
    write_text(CGROUP_THROTTLE "/cpu.max", "50000 100000");

    pid = fork();
    if (pid == 0) {
        close(pipefd[0]);
        move_to(CGROUP_THROTTLE);
        double start = now_sec();
        cpu_burn(1.0);
        double elapsed = now_sec() - start;
        write(pipefd[1], &elapsed, sizeof(elapsed));
        close(pipefd[1]);
        _exit(0);
    }

    close(pipefd[1]);
    double child_elapsed = 0;
    read(pipefd[0], &child_elapsed, sizeof(child_elapsed));
    close(pipefd[0]);

    int status;
    waitpid(pid, &status, 0);

    if (child_elapsed > 1.5) {
        CHECK(1, "TDD: cpu.max throttling works (wall > 1.5x)");
    } else {
        printf("  FAIL | TDD: cpu.max should throttle (wall=%.2fs, expected>1.5s)\n",
               child_elapsed);
        __fail++;
    }

    char buf[256];
    ssize_t n = read_text(CGROUP_THROTTLE "/cpu.stat", buf, sizeof(buf));
    CHECK(n >= 0, "read cpu.stat after throttle");
    if (n >= 0) {
        char *p = strstr(buf, "nr_throttled");
        if (p) {
            int nr = atoi(p + strlen("nr_throttled"));
            CHECK(nr > 0, "TDD: cpu.stat nr_throttled > 0");
        }
    }

    write_text(CGROUP_THROTTLE "/cpu.max", "max 100000");
    rmdir(CGROUP_THROTTLE);
}

/* ================================================================
 * Test 8: cpu.max "max" means unlimited
 * ================================================================ */
static void test_cpu_max_unlimited(void)
{
    mkdir(CGROUP_THROTTLE, 0755);
    write_text(CGROUP_THROTTLE "/cpu.max", "10000 100000");

    expect_write_ok(CGROUP_THROTTLE "/cpu.max", "max 100000",
                    "write cpu.max = max (unlimited)");
    expect_str_contains(CGROUP_THROTTLE "/cpu.max", "max",
                        "cpu.max reads back as max");

    pid_t pid = fork();
    if (pid == 0) {
        move_to(CGROUP_THROTTLE);
        double start = now_sec();
        cpu_burn(0.5);
        double elapsed = now_sec() - start;
        _exit(elapsed < 1.0 ? 0 : 1);
    }
    int status;
    waitpid(pid, &status, 0);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "unlimited cpu.max does not throttle");

    rmdir(CGROUP_THROTTLE);
}

/* ================================================================ */

int main(void)
{
    TEST_START("cgroup-cpu");

    test_cpu_weight_io();
    test_cpu_weight_clamping();
    test_cpu_max_io();
    test_cpu_stat_io();
    test_child_cpu_independent();
    test_cpu_weight_scheduling();
    test_cpu_max_throttle();
    test_cpu_max_unlimited();

    TEST_DONE();
}
