#define _GNU_SOURCE
#include <errno.h>
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/utsname.h>
#include <sys/wait.h>
#include <unistd.h>

/* ------------------------------------------------------------------ */
/* Fallback definitions for older / minimal libc headers               */
/* ------------------------------------------------------------------ */
#ifndef SYS_unshare
#define SYS_unshare 97
#endif
#ifndef SYS_sethostname
#define SYS_sethostname 161
#endif
#ifndef SYS_setdomainname
#define SYS_setdomainname 162
#endif
#ifndef CLONE_NEWUTS
#define CLONE_NEWUTS 0x04000000
#endif
#ifndef CLONE_NEWNS
#define CLONE_NEWNS 0x00020000
#endif

/* ------------------------------------------------------------------ */
/* Test helpers                                                        */
/* ------------------------------------------------------------------ */
static int passed;
static int failed;

static void note_pass(const char *name)
{
    printf("  PASS: %s\n", name);
    passed++;
}

static void note_fail(const char *name, const char *detail)
{
    printf("  FAIL: %s: %s\n", name, detail);
    failed++;
}

static int unshare_raw(int flags)
{
    return (int)syscall(SYS_unshare, flags);
}

static int sethostname_raw(const char *name, size_t len)
{
    return (int)syscall(SYS_sethostname, name, len);
}

static int setdomainname_raw(const char *name, size_t len)
{
    return (int)syscall(SYS_setdomainname, name, len);
}

/* ------------------------------------------------------------------ */
/* 1. unshare(CLONE_NEWUTS) succeeds                                  */
/* ------------------------------------------------------------------ */
static void test_unshare_newuts(void)
{
    errno = 0;
    int ret = unshare_raw(CLONE_NEWUTS);
    if (ret == 0) {
        note_pass("unshare(CLONE_NEWUTS) returns 0");
    } else {
        char buf[128];
        snprintf(buf, sizeof(buf), "ret=%d errno=%d (%s)", ret, errno, strerror(errno));
        note_fail("unshare(CLONE_NEWUTS)", buf);
    }
}

/* ------------------------------------------------------------------ */
/* 2. unshare rejects unsupported namespace flags                     */
/* ------------------------------------------------------------------ */
static void test_unshare_reject_newpid(void)
{
    errno = 0;
    int ret = unshare_raw(0x20000000); /* CLONE_NEWPID */
    if (ret == -1 && errno == EINVAL) {
        note_pass("unshare(NEWPID) returns EINVAL");
    } else {
        char buf[128];
        snprintf(buf, sizeof(buf), "ret=%d errno=%d (%s), expected -1/EINVAL",
                 ret, errno, strerror(errno));
        note_fail("unshare(NEWPID) EINVAL", buf);
    }
}

/* ------------------------------------------------------------------ */
/* 3. sethostname + uname round-trip in unshared namespace            */
/* ------------------------------------------------------------------ */
static void test_sethostname_uname(void)
{
    /* Enter a fresh UTS namespace */
    if (unshare_raw(CLONE_NEWUTS) != 0) {
        note_fail("sethostname+uname", "unshare failed, cannot run sub-test");
        return;
    }

    /* Set hostname to a known value */
    const char *hn = "test-hostname-42";
    errno = 0;
    if (sethostname_raw(hn, strlen(hn)) != 0) {
        char buf[128];
        snprintf(buf, sizeof(buf), "sethostname errno=%d (%s)", errno, strerror(errno));
        note_fail("sethostname", buf);
        return;
    }
    note_pass("sethostname succeeds");

    /* Read back via uname */
    struct utsname uts;
    errno = 0;
    if (uname(&uts) != 0) {
        char buf[128];
        snprintf(buf, sizeof(buf), "uname errno=%d (%s)", errno, strerror(errno));
        note_fail("uname after sethostname", buf);
        return;
    }

    if (strcmp(uts.nodename, hn) == 0) {
        note_pass("uname nodename matches sethostname value");
    } else {
        char buf[256];
        snprintf(buf, sizeof(buf), "nodename=\"%s\" expected=\"%s\"",
                 uts.nodename, hn);
        note_fail("uname nodename", buf);
    }
}

/* ------------------------------------------------------------------ */
/* 4. setdomainname + uname round-trip                                */
/* ------------------------------------------------------------------ */
static void test_setdomainname_uname(void)
{
    if (unshare_raw(CLONE_NEWUTS) != 0) {
        note_fail("setdomainname+uname", "unshare failed");
        return;
    }

    const char *dn = "test-domain-7";
    errno = 0;
    if (setdomainname_raw(dn, strlen(dn)) != 0) {
        char buf[128];
        snprintf(buf, sizeof(buf), "setdomainname errno=%d (%s)", errno, strerror(errno));
        note_fail("setdomainname", buf);
        return;
    }
    note_pass("setdomainname succeeds");

    struct utsname uts;
    if (uname(&uts) != 0) {
        note_fail("uname after setdomainname", "uname syscall failed");
        return;
    }

    if (strcmp(uts.domainname, dn) == 0) {
        note_pass("uname domainname matches setdomainname value");
    } else {
        char buf[256];
        snprintf(buf, sizeof(buf), "domainname=\"%s\" expected=\"%s\"",
                 uts.domainname, dn);
        note_fail("uname domainname", buf);
    }
}

/* ------------------------------------------------------------------ */
/* 5. After fork, child inherits parent's hostname (clone_ns)        */
/* ------------------------------------------------------------------ */
static void test_fork_inherits_hostname(void)
{
    if (unshare_raw(CLONE_NEWUTS) != 0) {
        note_fail("fork-inherits", "unshare failed");
        return;
    }

    const char *hn = "parent-hn";
    if (sethostname_raw(hn, strlen(hn)) != 0) {
        note_fail("fork-inherits setup", "sethostname failed");
        return;
    }

    int pipefd[2];
    if (pipe(pipefd) != 0) {
        note_fail("fork-inherits", "pipe failed");
        return;
    }

    pid_t pid = fork();
    if (pid < 0) {
        note_fail("fork-inherits", "fork failed");
        close(pipefd[0]);
        close(pipefd[1]);
        return;
    }

    if (pid == 0) {
        /* Child: send nodename back to parent via pipe */
        close(pipefd[0]);
        struct utsname uts;
        int ok = (uname(&uts) == 0 && strcmp(uts.nodename, hn) == 0);
        const char *name = ok ? hn : (uname(&uts) == 0 ? uts.nodename : "ERROR");
        write(pipefd[1], name, strlen(name) + 1);
        close(pipefd[1]);
        _exit(0);
    }

    /* Parent: read child's nodename */
    close(pipefd[1]);
    char child_name[65];
    ssize_t n = read(pipefd[0], child_name, sizeof(child_name));
    close(pipefd[0]);

    int status;
    waitpid(pid, &status, 0);

    if (n > 0 && strcmp(child_name, hn) == 0) {
        note_pass("fork child inherits parent hostname");
    } else {
        char buf[256];
        snprintf(buf, sizeof(buf), "child nodename=\"%s\" expected=\"%s\" n=%zd",
                 n > 0 ? child_name : "(none)", hn, n);
        note_fail("fork-inherits", buf);
    }
}

/* ------------------------------------------------------------------ */
/* 6. Shared namespace: child hostname change visible to parent       */
/* ------------------------------------------------------------------ */
static void test_shared_namespace_propagation(void)
{
    if (unshare_raw(CLONE_NEWUTS) != 0) {
        note_fail("shared-ns", "unshare failed");
        return;
    }

    const char *parent_hn = "parent-shared";
    if (sethostname_raw(parent_hn, strlen(parent_hn)) != 0) {
        note_fail("shared-ns setup", "parent sethostname failed");
        return;
    }

    int pipefd[2];
    if (pipe(pipefd) != 0) {
        note_fail("shared-ns", "pipe failed");
        return;
    }

    pid_t pid = fork();
    if (pid < 0) {
        note_fail("shared-ns", "fork failed");
        close(pipefd[0]);
        close(pipefd[1]);
        return;
    }

    if (pid == 0) {
        close(pipefd[0]);

        /* Child: change to its own hostname in the shared namespace */
        const char *child_hn = "child-shared";
        if (sethostname_raw(child_hn, strlen(child_hn)) != 0) {
            write(pipefd[1], "SETFAIL", 8);
            close(pipefd[1]);
            _exit(1);
        }

        /* Verify child sees its own hostname */
        struct utsname uts;
        if (uname(&uts) != 0) {
            write(pipefd[1], "UNAMEFAIL", 10);
            close(pipefd[1]);
            _exit(1);
        }

        write(pipefd[1], uts.nodename, strlen(uts.nodename) + 1);
        close(pipefd[1]);
        _exit(0);
    }

    /* Parent */
    close(pipefd[1]);
    char child_name[65];
    ssize_t n = read(pipefd[0], child_name, sizeof(child_name));
    close(pipefd[0]);

    int status;
    waitpid(pid, &status, 0);

    if (n <= 0) {
        note_fail("shared-ns", "failed to read child nodename");
        return;
    }

    /* Parent should see the child's change because they share the
     * same UTS namespace via Arc. */
    struct utsname uts;
    if (uname(&uts) != 0) {
        note_fail("shared-ns parent uname", "uname failed");
        return;
    }

    const char *child_expected = "child-shared";
    int child_ok = (strcmp(child_name, child_expected) == 0);
    int parent_ok = (strcmp(uts.nodename, child_expected) == 0);

    if (child_ok && parent_ok) {
        note_pass("shared namespace: parent sees child hostname change");
    } else {
        char buf[256];
        snprintf(buf, sizeof(buf),
                 "child=\"%s\"(exp=%s) parent=\"%s\"(exp=%s)",
                 child_name, child_expected, uts.nodename, child_expected);
        note_fail("shared-ns", buf);
    }
}

/* ------------------------------------------------------------------ */
/* main                                                                */
/* ------------------------------------------------------------------ */
int main(void)
{
    printf("=== bug-unshare-uts ===\n");

    test_unshare_newuts();
    test_unshare_reject_newpid();
    test_sethostname_uname();
    test_setdomainname_uname();
    test_fork_inherits_hostname();
    test_shared_namespace_propagation();

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("ALL TESTS PASSED\n");
        return 0;
    }
    printf("SOME TESTS FAILED\n");
    return 1;
}
