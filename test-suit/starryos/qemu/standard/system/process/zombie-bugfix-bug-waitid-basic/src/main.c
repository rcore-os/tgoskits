#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef P_ALL
#define P_ALL 0
#endif
#ifndef P_PID
#define P_PID 1
#endif
#ifndef P_PGID
#define P_PGID 2
#endif
#ifndef WEXITED
#define WEXITED 0x00000004
#endif
#ifndef WNOWAIT
#define WNOWAIT 0x01000000
#endif

static int passed;
static int failed;

static void note_pass(const char *name)
{
    printf("PASS: %s\n", name);
    passed++;
}

static void note_fail(const char *name, const char *detail)
{
    printf("FAIL: %s: %s\n", name, detail);
    failed++;
}

static int waitid_raw(int idtype, pid_t id, siginfo_t *si, int options)
{
    return (int)syscall(SYS_waitid, idtype, id, si, options, NULL);
}

/* 1. P_PID + WEXITED: fork child that _exit(7), waitid and check siginfo */
static void test_ppid_wexited(void)
{
    pid_t pid = fork();
    if (pid < 0) {
        note_fail("P_PID WEXITED", "fork failed");
        return;
    }
    if (pid == 0)
        _exit(7);

    siginfo_t si;
    memset(&si, 0xa5, sizeof(si));
    errno = 0;
    int ret = waitid_raw(P_PID, pid, &si, WEXITED);
    if (ret != 0) {
        char buf[160];
        snprintf(buf, sizeof(buf), "ret=%d errno=%d (%s)", ret, errno, strerror(errno));
        note_fail("P_PID WEXITED", buf);
        return;
    }
    if (si.si_signo != SIGCHLD || si.si_code != CLD_EXITED || si.si_pid != pid ||
        si.si_status != 7) {
        char buf[256];
        snprintf(buf, sizeof(buf),
                 "signo=%d code=%d pid=%d status=%d, expected SIGCHLD/CLD_EXITED/%d/7",
                 si.si_signo, si.si_code, si.si_pid, si.si_status, pid);
        note_fail("P_PID WEXITED siginfo", buf);
        return;
    }

    /* Verify reaped: waitpid should fail with ECHILD */
    int status;
    errno = 0;
    pid_t wret = waitpid(pid, &status, WNOHANG);
    if (wret == -1 && errno == ECHILD) {
        note_pass("P_PID WEXITED reaps child");
    } else {
        char buf[160];
        snprintf(buf, sizeof(buf), "waitpid ret=%ld errno=%d, expected -1/ECHILD",
                 (long)wret, errno);
        note_fail("P_PID WEXITED reap", buf);
    }
}

/* 2. P_ALL + WEXITED */
static void test_pall_wexited(void)
{
    pid_t pid = fork();
    if (pid < 0) {
        note_fail("P_ALL WEXITED", "fork failed");
        return;
    }
    if (pid == 0)
        _exit(3);

    siginfo_t si;
    memset(&si, 0, sizeof(si));
    errno = 0;
    int ret = waitid_raw(P_ALL, 0, &si, WEXITED);
    if (ret != 0) {
        char buf[160];
        snprintf(buf, sizeof(buf), "ret=%d errno=%d (%s)", ret, errno, strerror(errno));
        note_fail("P_ALL WEXITED", buf);
        return;
    }
    if (si.si_signo != SIGCHLD || si.si_code != CLD_EXITED || si.si_pid != pid ||
        si.si_status != 3) {
        char buf[256];
        snprintf(buf, sizeof(buf),
                 "signo=%d code=%d pid=%d status=%d, expected SIGCHLD/CLD_EXITED/%d/3",
                 si.si_signo, si.si_code, si.si_pid, si.si_status, pid);
        note_fail("P_ALL WEXITED siginfo", buf);
        return;
    }
    note_pass("P_ALL WEXITED");
}

/* 3. P_PGID + WEXITED with an explicit child process group */
static void test_ppgid_wexited(void)
{
    int ready[2];
    if (pipe(ready) != 0) {
        note_fail("P_PGID WEXITED", "pipe failed");
        return;
    }

    pid_t pid = fork();
    if (pid < 0) {
        note_fail("P_PGID WEXITED", "fork failed");
        close(ready[0]);
        close(ready[1]);
        return;
    }
    if (pid == 0) {
        close(ready[0]);
        int ok = setpgid(0, 0);
        char marker = ok == 0 ? '1' : '0';
        ssize_t written = write(ready[1], &marker, 1);
        (void)written;
        close(ready[1]);
        _exit(ok == 0 ? 17 : 91);
    }

    close(ready[1]);
    char marker = 0;
    ssize_t nread = read(ready[0], &marker, 1);
    close(ready[0]);
    if (nread != 1 || marker != '1') {
        waitpid(pid, NULL, 0);
        note_fail("P_PGID WEXITED", "child failed to create process group");
        return;
    }

    siginfo_t si;
    memset(&si, 0, sizeof(si));
    errno = 0;
    int ret = waitid_raw(P_PGID, pid, &si, WEXITED);
    if (ret != 0) {
        char buf[160];
        snprintf(buf, sizeof(buf), "ret=%d errno=%d (%s)", ret, errno, strerror(errno));
        note_fail("P_PGID WEXITED", buf);
        waitpid(pid, NULL, 0);
        return;
    }
    if (si.si_signo != SIGCHLD || si.si_code != CLD_EXITED || si.si_pid != pid ||
        si.si_status != 17) {
        char buf[256];
        snprintf(buf, sizeof(buf),
                 "signo=%d code=%d pid=%d status=%d, expected SIGCHLD/CLD_EXITED/%d/17",
                 si.si_signo, si.si_code, si.si_pid, si.si_status, pid);
        note_fail("P_PGID WEXITED siginfo", buf);
        return;
    }

    errno = 0;
    pid_t wret = waitpid(pid, NULL, WNOHANG);
    if (wret == -1 && errno == ECHILD) {
        note_pass("P_PGID WEXITED reaps child");
    } else {
        char buf[160];
        snprintf(buf, sizeof(buf), "waitpid ret=%ld errno=%d, expected -1/ECHILD",
                 (long)wret, errno);
        note_fail("P_PGID WEXITED reap", buf);
    }
}

/* 4. P_PGID with id 0 waits in the caller's current process group */
static void test_ppgid_zero_current_group(void)
{
    pid_t pid = fork();
    if (pid < 0) {
        note_fail("P_PGID id 0", "fork failed");
        return;
    }
    if (pid == 0)
        _exit(23);

    siginfo_t si;
    memset(&si, 0, sizeof(si));
    errno = 0;
    int ret = waitid_raw(P_PGID, 0, &si, WEXITED);
    if (ret != 0) {
        char buf[160];
        snprintf(buf, sizeof(buf), "ret=%d errno=%d (%s)", ret, errno, strerror(errno));
        note_fail("P_PGID id 0", buf);
        waitpid(pid, NULL, 0);
        return;
    }
    if (si.si_signo == SIGCHLD && si.si_code == CLD_EXITED && si.si_pid == pid &&
        si.si_status == 23) {
        note_pass("P_PGID id 0 uses current process group");
    } else {
        char buf[256];
        snprintf(buf, sizeof(buf),
                 "signo=%d code=%d pid=%d status=%d, expected SIGCHLD/CLD_EXITED/%d/23",
                 si.si_signo, si.si_code, si.si_pid, si.si_status, pid);
        note_fail("P_PGID id 0 siginfo", buf);
    }
}

/* 5. WNOHANG: child still running -> return 0, si_pid==0 */
static void test_wnohang(void)
{
    pid_t pid = fork();
    if (pid < 0) {
        note_fail("WNOHANG", "fork failed");
        return;
    }
    if (pid == 0) {
        usleep(200000);
        _exit(0);
    }

    siginfo_t si;
    memset(&si, 0xff, sizeof(si));
    errno = 0;
    int ret = waitid_raw(P_PID, pid, &si, WEXITED | WNOHANG);
    int saved_errno = errno;
    if (ret != 0) {
        char buf[160];
        snprintf(buf, sizeof(buf), "ret=%d errno=%d (%s)", ret, saved_errno, strerror(saved_errno));
        note_fail("WNOHANG ret", buf);
        /* reap child */
        waitpid(pid, NULL, 0);
        return;
    }
    if (si.si_pid != 0) {
        char buf[160];
        snprintf(buf, sizeof(buf), "si_pid=%d, expected 0", si.si_pid);
        note_fail("WNOHANG si_pid", buf);
    } else {
        note_pass("WNOHANG returns 0 with si_pid==0");
    }

    /* Reap the child */
    waitpid(pid, NULL, 0);
}

/* 6. WNOWAIT: query siginfo without reaping, then reap with waitpid */
static void test_wnowait(void)
{
    pid_t pid = fork();
    if (pid < 0) {
        note_fail("WNOWAIT", "fork failed");
        return;
    }
    if (pid == 0)
        _exit(42);

    /* Wait for child to exit */
    usleep(50000);

    /* First call: WNOWAIT — should get siginfo but NOT reap */
    siginfo_t si;
    memset(&si, 0, sizeof(si));
    errno = 0;
    int ret = waitid_raw(P_PID, pid, &si, WEXITED | WNOWAIT);
    if (ret != 0) {
        char buf[160];
        snprintf(buf, sizeof(buf), "WNOWAIT ret=%d errno=%d (%s)", ret, errno, strerror(errno));
        note_fail("WNOWAIT waitid", buf);
        waitpid(pid, NULL, 0);
        return;
    }
    if (si.si_signo != SIGCHLD || si.si_code != CLD_EXITED || si.si_pid != pid ||
        si.si_status != 42) {
        char buf[256];
        snprintf(buf, sizeof(buf),
                 "signo=%d code=%d pid=%d status=%d, expected SIGCHLD/CLD_EXITED/%d/42",
                 si.si_signo, si.si_code, si.si_pid, si.si_status, pid);
        note_fail("WNOWAIT siginfo", buf);
        waitpid(pid, NULL, 0);
        return;
    }

    /* Child should still be reapable via waitpid */
    int status = 0;
    errno = 0;
    pid_t wret = waitpid(pid, &status, 0);
    if (wret == pid && WIFEXITED(status) && WEXITSTATUS(status) == 42) {
        note_pass("WNOWAIT does not reap, waitpid succeeds after");
    } else {
        char buf[256];
        snprintf(buf, sizeof(buf),
                 "waitpid ret=%ld errno=%d status=0x%x, expected pid=%d exit=42",
                 (long)wret, errno, status, pid);
        note_fail("WNOWAIT reap", buf);
    }
}

/* 7. Error: ECHILD for non-child pid */
static void test_echild(void)
{
    siginfo_t si;
    memset(&si, 0, sizeof(si));
    errno = 0;
    /* pid 1 is init, not our child */
    int ret = waitid_raw(P_PID, 1, &si, WEXITED);
    if (ret == -1 && errno == ECHILD) {
        note_pass("P_PID non-child returns ECHILD");
    } else {
        char buf[160];
        snprintf(buf, sizeof(buf), "ret=%d errno=%d (%s), expected -1/ECHILD",
                 ret, errno, strerror(errno));
        note_fail("P_PID non-child ECHILD", buf);
    }
}

/* 8. Error: EINVAL for bad idtype */
static void test_einval_idtype(void)
{
    siginfo_t si;
    memset(&si, 0, sizeof(si));
    errno = 0;
    int ret = waitid_raw(99, 0, &si, WEXITED);
    if (ret == -1 && errno == EINVAL) {
        note_pass("bad idtype returns EINVAL");
    } else {
        char buf[160];
        snprintf(buf, sizeof(buf), "ret=%d errno=%d (%s), expected -1/EINVAL",
                 ret, errno, strerror(errno));
        note_fail("bad idtype EINVAL", buf);
    }
}

/* 9. Error: EINVAL for missing WEXITED */
static void test_einval_no_wexited(void)
{
    pid_t pid = fork();
    if (pid < 0) {
        note_fail("no WEXITED", "fork failed");
        return;
    }
    if (pid == 0)
        _exit(0);

    siginfo_t si;
    memset(&si, 0, sizeof(si));
    errno = 0;
    int ret = waitid_raw(P_PID, pid, &si, WNOHANG);
    int saved_errno = errno;
    if (ret == -1 && saved_errno == EINVAL) {
        note_pass("missing WEXITED returns EINVAL");
    } else {
        char buf[160];
        snprintf(buf, sizeof(buf), "ret=%d errno=%d (%s), expected -1/EINVAL",
                 ret, saved_errno, strerror(saved_errno));
        note_fail("missing WEXITED EINVAL", buf);
    }

    /* Reap child */
    waitpid(pid, NULL, 0);
}

/* 10. Error: EINVAL for a negative P_PGID id */
static void test_einval_negative_ppgid(void)
{
    siginfo_t si;
    memset(&si, 0, sizeof(si));
    errno = 0;
    int ret = waitid_raw(P_PGID, -1, &si, WEXITED);
    if (ret == -1 && errno == EINVAL) {
        note_pass("negative P_PGID id returns EINVAL");
    } else {
        char buf[160];
        snprintf(buf, sizeof(buf), "ret=%d errno=%d (%s), expected -1/EINVAL",
                 ret, errno, strerror(errno));
        note_fail("negative P_PGID id EINVAL", buf);
    }
}

/* 11. infop == NULL: waitid should succeed and still reap the child */
static void test_null_infop(void)
{
    pid_t pid = fork();
    if (pid < 0) {
        note_fail("NULL infop", "fork failed");
        return;
    }
    if (pid == 0)
        _exit(13);

    errno = 0;
    int ret = waitid_raw(P_PID, pid, NULL, WEXITED);
    if (ret != 0) {
        char buf[160];
        snprintf(buf, sizeof(buf), "ret=%d errno=%d (%s)", ret, errno, strerror(errno));
        note_fail("NULL infop ret", buf);
        return;
    }

    /* Verify reaped: waitpid should fail with ECHILD */
    int status;
    errno = 0;
    pid_t wret = waitpid(pid, &status, WNOHANG);
    if (wret == -1 && errno == ECHILD) {
        note_pass("NULL infop reaps child");
    } else {
        char buf[160];
        snprintf(buf, sizeof(buf), "waitpid ret=%ld errno=%d, expected -1/ECHILD",
                 (long)wret, errno);
        note_fail("NULL infop reap", buf);
    }
}

int main(void)
{
    printf("=== bug-waitid-basic ===\n");

    test_ppid_wexited();
    test_pall_wexited();
    test_ppgid_wexited();
    test_ppgid_zero_current_group();
    test_wnohang();
    test_wnowait();
    test_echild();
    test_einval_idtype();
    test_einval_no_wexited();
    test_einval_negative_ppgid();
    test_null_infop();

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("ALL TESTS PASSED\n");
        return 0;
    }
    printf("SOME TESTS FAILED\n");
    return 1;
}
