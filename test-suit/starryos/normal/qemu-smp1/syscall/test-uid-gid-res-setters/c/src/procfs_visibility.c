#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static int waitpid_safely_pv(pid_t pid, int *status)
{
    pid_t r = waitpid(pid, status, 0);
    return r == pid ? 0 : -1;
}

static int parse_proc_id_line(const char *prefix,
                               uint32_t *r, uint32_t *e, uint32_t *s, uint32_t *fs)
{
    FILE *f = fopen("/proc/self/status", "r");
    if (!f) return -1;
    char line[256];
    int found = -1;
    while (fgets(line, sizeof line, f)) {
        if (strncmp(line, prefix, strlen(prefix)) == 0) {
            if (sscanf(line + strlen(prefix), "%u %u %u %u", r, e, s, fs) == 4) {
                found = 0;
            }
            break;
        }
    }
    fclose(f);
    return found;
}

static int parse_proc_value_line(const char *prefix, uint32_t *value)
{
    FILE *f = fopen("/proc/self/status", "r");
    if (!f) return -1;
    char line[256];
    int found = -1;
    while (fgets(line, sizeof line, f)) {
        if (strncmp(line, prefix, strlen(prefix)) == 0) {
            if (sscanf(line + strlen(prefix), "%u", value) == 1) {
                found = 0;
            }
            break;
        }
    }
    fclose(f);
    return found;
}

static void proc_uid_baseline(void)
{
    if (getuid() != 0) {
        printf("  procfs (a) skip\n");
        return;
    }
    uint32_t r, e, s, fs;
    int rc = parse_proc_id_line("Uid:", &r, &e, &s, &fs);
    CHECK(rc == 0 && r == 0 && e == 0 && s == 0 && fs == 0,
          "procfs (a) baseline root: Uid 0 0 0 0");
}

static void proc_uid_after_setresuid(void)
{
    if (getuid() != 0) {
        printf("  procfs (b) skip\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresuid(1000, 2000, 3000) != 0) _exit(99);
        uint32_t r, e, s, fs;
        if (parse_proc_id_line("Uid:", &r, &e, &s, &fs) != 0) _exit(98);
        if (r != 1000 || e != 2000 || s != 3000 || fs != 2000) _exit(1);
        _exit(0);
    }
    int status;
    waitpid_safely_pv(pid, &status);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "procfs (b) setresuid(1k,2k,3k) -> Uid 1000 2000 3000 2000");
}

static void proc_uid_after_setresuid_nochg(void)
{
    if (getuid() != 0) {
        printf("  procfs (c) skip\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresuid((uid_t)-1, 4000, (uid_t)-1) != 0) _exit(99);
        uint32_t r, e, s, fs;
        if (parse_proc_id_line("Uid:", &r, &e, &s, &fs) != 0) _exit(98);
        if (r != 0 || e != 4000 || s != 0 || fs != 4000) _exit(1);
        _exit(0);
    }
    int status;
    waitpid_safely_pv(pid, &status);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "procfs (c) setresuid(-1,4000,-1) -> 0 4000 0 4000");
}

static void proc_gid_after_setresgid(void)
{
    if (getuid() != 0) {
        printf("  procfs (d) skip\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresgid(1000, 2000, 3000) != 0) _exit(99);
        uint32_t r, e, s, fs;
        if (parse_proc_id_line("Gid:", &r, &e, &s, &fs) != 0) _exit(98);
        if (r != 1000 || e != 2000 || s != 3000 || fs != 2000) _exit(1);
        _exit(0);
    }
    int status;
    waitpid_safely_pv(pid, &status);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "procfs (d) setresgid(1k,2k,3k) -> Gid 1000 2000 3000 2000");
}

static void proc_compound_setres(void)
{
    if (getuid() != 0) {
        printf("  procfs (e) skip\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresgid(400, 500, 600) != 0) _exit(99);
        if (setresuid(100, 200, 300) != 0) _exit(98);
        uint32_t ur, ue, us, ufs;
        uint32_t gr, ge, gs, gfs;
        if (parse_proc_id_line("Uid:", &ur, &ue, &us, &ufs) != 0) _exit(97);
        if (parse_proc_id_line("Gid:", &gr, &ge, &gs, &gfs) != 0) _exit(96);
        if (ur != 100 || ue != 200 || us != 300 || ufs != 200) _exit(1);
        if (gr != 400 || ge != 500 || gs != 600 || gfs != 500) _exit(2);
        _exit(0);
    }
    int status;
    waitpid_safely_pv(pid, &status);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "procfs (e) compound setresuid + setresgid");
}

static void proc_gid_baseline(void)
{
    if (getuid() != 0) {
        printf("  procfs (f) skip\n");
        return;
    }
    uint32_t r, e, s, fs;
    int rc = parse_proc_id_line("Gid:", &r, &e, &s, &fs);
    CHECK(rc == 0 && r == 0 && e == 0 && s == 0 && fs == 0,
          "procfs (f) baseline root: Gid 0 0 0 0");
}

static void proc_dumpable_visibility(void)
{
    if (getuid() != 0) {
        printf("  procfs (g) skip\n");
        return;
    }

    pid_t pid = fork();
    if (pid == 0) {
        uint32_t dumpable = 0;
        if (parse_proc_value_line("Dumpable:", &dumpable) != 0 || dumpable != 1) _exit(1);
        if (prctl(PR_SET_DUMPABLE, 0) != 0) _exit(2);
        if (parse_proc_value_line("Dumpable:", &dumpable) != 0 || dumpable != 0) _exit(3);
        if (prctl(PR_SET_DUMPABLE, 1) != 0) _exit(4);
        if (parse_proc_value_line("Dumpable:", &dumpable) != 0 || dumpable != 1) _exit(5);
        _exit(0);
    }

    int status;
    waitpid_safely_pv(pid, &status);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "procfs (g) Dumpable line tracks PR_SET_DUMPABLE");
}

int procfs_visibility_run(void)
{
    printf("\n----- procfs_visibility -----\n");
    proc_uid_baseline();
    proc_uid_after_setresuid();
    proc_uid_after_setresuid_nochg();
    proc_gid_after_setresgid();
    proc_compound_setres();
    proc_gid_baseline();
    proc_dumpable_visibility();
    printf("  ----- procfs_visibility: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
