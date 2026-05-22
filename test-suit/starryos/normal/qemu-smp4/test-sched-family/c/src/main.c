#define _GNU_SOURCE
#include "test_framework.h"
#include <sched.h>
#include <sys/resource.h>
#include <unistd.h>
#include <errno.h>
#include <string.h>
#include <stdio.h>
#include <signal.h> 


/*
 * Read the real UID and effective UID of a target process from
 * /proc/<pid>/status.
 *
 * Linux exposes process credentials in the "Uid:" field:
 *
 *   Uid:    <real> <effective> <saved> <fs>
 *
 * This helper is used to emulate Linux permission checks for
 * operations such as sched_setaffinity(), where permission is
 * granted if:
 *
 *   caller.euid == target.ruid
 *   OR
 *   caller.euid == target.euid
 *   OR
 *   caller has CAP_SYS_NICE
 *
 * Parameters:
 *   pid   - target process ID
 *   ruid  - output pointer for target real UID
 *   euid  - output pointer for target effective UID
 *
 * Return:
 *    0 -> success
 *   -1 -> failed to read or parse UID information
 */
static int read_uids(pid_t pid, uid_t *ruid, uid_t *euid)
{
    char path[64];
    snprintf(path, sizeof(path), "/proc/%d/status", pid);

    FILE *f = fopen(path, "r");
    if (!f)
        return -1;

    char line[256];
    int found = 0;

    while (fgets(line, sizeof(line), f)) {
        if (strncmp(line, "Uid:", 4) == 0) {
            unsigned int r, e, s, fs;

            if (sscanf(line + 4, "%u\t%u\t%u\t%u",
                       &r, &e, &s, &fs) >= 2) {
                *ruid = (uid_t)r;
                *euid = (uid_t)e;
                found = 1;
                break;
            }
        }
    }

    fclose(f);
    return found ? 0 : -1;
}


/*
 * Check whether the current process has CAP_SYS_NICE in its
 * effective capability set.
 *
 * Linux exposes process capabilities through /proc/self/status:
 *
 *   CapEff: <hex capability bitmap>
 *
 * CAP_SYS_NICE is capability number 23, which permits privileged
 * scheduling operations such as changing another task's CPU affinity.
 *
 * Return:
 *   1 -> CAP_SYS_NICE is present
 *   0 -> CAP_SYS_NICE is absent or capability information is unavailable
 */
static int has_cap_sys_nice(void)
{
    FILE *f = fopen("/proc/self/status", "r");
    if (!f) return 0;

    char line[256];
    int ok = 0;

    while (fgets(line, sizeof(line), f)) {
        if (strncmp(line, "CapEff:", 7) == 0) {
            unsigned long long cap = 0;

            if (sscanf(line + 7, "%llx", &cap) == 1) {
                /* CAP_SYS_NICE == 23 */
                if (cap & (1ULL << 23))
                    ok = 1;
            }
            break;
        }
    }

    fclose(f);
    return ok;
}

/*
 *   test-sched-family 对比测试:
 *   Linux/WSL 行为 vs StarryOS 行为
 */

int main(void)
{
    TEST_START("sched_setaffinity / sched_getaffinity");

    // 测试 sched_setaffinity() / sched_getaffinity 在当前线程上的正常行为
    {

        // 常规用例：将当前线程绑定到 CPU 0，并验证 getaffinity 返回正确的结果
       {
            cpu_set_t mask, readback;
            long nprocs = sysconf(_SC_NPROCESSORS_ONLN);
            if (nprocs < 1) nprocs = 1;

            CPU_ZERO(&mask);
            CPU_SET(0, &mask);

            CHECK_RET(sched_setaffinity(0, sizeof(mask), &mask), 0, "setaffinity current pid to cpu0");

            memset(&readback, 0, sizeof(readback));
            CHECK_RET(sched_getaffinity(0, sizeof(readback), &readback), 0, "getaffinity current pid");
            CHECK(CPU_ISSET(0, &readback), "getaffinity result contains cpu0");
        } 

        // EFAULT: supplied memory address was invalid
        {
            CHECK_ERR(sched_setaffinity(0, sizeof(cpu_set_t), (cpu_set_t *)0x1), EFAULT, "setaffinity with invalid pointer returns EFAULT");
            CHECK_ERR(sched_getaffinity(0, sizeof(cpu_set_t), (cpu_set_t *)0x1), EFAULT, "getaffinity with invalid pointer returns EFAULT");
        }

        // EINVAL: affinity mask contains no online CPUs
        {
            long nprocs = sysconf(_SC_NPROCESSORS_ONLN);
            if (nprocs < 1) nprocs = 1;
            cpu_set_t mask;
            CPU_ZERO(&mask);
            CHECK_ERR(sched_setaffinity(0, sizeof(mask), &mask), EINVAL, "setaffinity with mask containing no online CPUs returns EINVAL");
        }

        // EINVAL: cpusetsize smaller than the kernel affinity mask size
        {
            cpu_set_t mask;
            CPU_ZERO(&mask);
            CPU_SET(0, &mask);
            CHECK_ERR(sched_getaffinity(0, 0, &mask), EINVAL, "getaffinity with too small cpusetsize returns EINVAL");
        }

        // EPERM: permission semantics test for changing another process's affinity
        {
            cpu_set_t mask;
            CPU_ZERO(&mask);
            CPU_SET(0, &mask);

            pid_t target = 1;
            uid_t target_ruid = (uid_t)-1, target_euid = (uid_t)-1;
            int have_uids = (read_uids(target, &target_ruid, &target_euid) == 0);
            uid_t my_euid = geteuid();

            if (have_uids) {
                if (my_euid == target_ruid || my_euid == target_euid || has_cap_sys_nice()) {
                    CHECK_RET(sched_setaffinity(target, sizeof(mask), &mask), 0, "setaffinity pid 1 when caller has matching UID or CAP_SYS_NICE returns 0");
                } else {
                    CHECK_ERR(sched_setaffinity(target, sizeof(mask), &mask), EPERM, "setaffinity pid 1 without matching UID or CAP_SYS_NICE returns EPERM");
                }
            } else {
                // fallback: if root, allow; else expect EPERM
                if (my_euid == 0) {
                    CHECK_RET(sched_setaffinity(target, sizeof(mask), &mask), 0, "setaffinity pid 1 as root (fallback) returns 0");
                } else {
                    CHECK_ERR(sched_setaffinity(target, sizeof(mask), &mask), EPERM, "setaffinity pid 1 without privileges (fallback) returns EPERM");
                }
            }
        }

        // ESRCH: non-existent pid should return ESRCH
        {
            cpu_set_t mask;
            CPU_ZERO(&mask);
            CPU_SET(0, &mask);
            pid_t fake = 999999;
            CHECK_ERR(sched_setaffinity(fake, sizeof(mask), &mask), ESRCH, "setaffinity for non-existent pid returns ESRCH");
            CHECK_ERR(sched_getaffinity(fake, sizeof(mask), &mask), ESRCH, "getaffinity for non-existent pid returns ESRCH");
        }
    }

    // sched_yield() 应当成功返回 0
    {
        CHECK_RET(sched_yield(), 0, "sched_yield returns 0");
    }

    TEST_DONE();

    // 测试 sched_setscheduler / sched_getscheduler
    {
        struct sched_param sp;
        int ret, pol;

        // 负 pid -> EINVAL
        memset(&sp, 0, sizeof(sp));
        sp.sched_priority = 0;
        CHECK_ERR(sched_setscheduler(-5, SCHED_OTHER, &sp), EINVAL, "sched_setscheduler with negative pid returns EINVAL");
        CHECK_ERR(sched_getscheduler(-5), EINVAL, "sched_getscheduler with negative pid returns EINVAL");

        // NULL param -> EINVAL (for sched_setscheduler)
        CHECK_ERR(sched_setscheduler(0, SCHED_OTHER, NULL), EINVAL, "sched_setscheduler with NULL param returns EINVAL");

        // invalid policy -> EINVAL
        memset(&sp, 0, sizeof(sp));
        CHECK_ERR(sched_setscheduler(0, 0xdeadbeef, &sp), EINVAL, "sched_setscheduler with invalid policy returns EINVAL");

        // SCHED_OTHER with non-zero priority -> EINVAL
        sp.sched_priority = 1;
        CHECK_ERR(sched_setscheduler(0, SCHED_OTHER, &sp), EINVAL, "sched_setscheduler SCHED_OTHER with nonzero priority returns EINVAL");
        sp.sched_priority = 0;

        // sched_getscheduler for current thread (pid 0) returns non-negative policy
        pol = sched_getscheduler(0);
        CHECK(pol >= 0, "sched_getscheduler(0) returns non-negative policy");

        // nonexistent pid -> ESRCH
        pid_t fake = 999999;
        CHECK_ERR(sched_getscheduler(fake), ESRCH, "sched_getscheduler for non-existent pid returns ESRCH");
        CHECK_ERR(sched_setscheduler(fake, SCHED_OTHER, &sp), ESRCH, "sched_setscheduler for non-existent pid returns ESRCH");

        // Trying to set real-time policy without privileges -> EPERM (or success if root)
        int maxprio = sched_get_priority_max(SCHED_RR);
        if (maxprio < 1) maxprio = 1;
        sp.sched_priority = 1;
        if (geteuid() == 0) {
            ret = sched_setscheduler(0, SCHED_FIFO, &sp);
            CHECK_RET(ret, 0, "sched_setscheduler to SCHED_FIFO as root returns 0");
            // restore to SCHED_OTHER
            sp.sched_priority = 0;
            CHECK_RET(sched_setscheduler(0, SCHED_OTHER, &sp), 0, "restore to SCHED_OTHER as root returns 0");
        } else {
            CHECK_ERR(sched_setscheduler(0, SCHED_FIFO, &sp), EPERM, "sched_setscheduler to SCHED_FIFO without privileges returns EPERM");
        }

        // priority out of range -> EINVAL
        sp.sched_priority = maxprio + 1;
        CHECK_ERR(sched_setscheduler(0, SCHED_RR, &sp), EINVAL, "sched_setscheduler with priority > max returns EINVAL");

        // setting SCHED_OTHER with zero priority should succeed for current thread
        sp.sched_priority = 0;
        ret = sched_setscheduler(0, SCHED_OTHER, &sp);
        CHECK_RET(ret, 0, "sched_setscheduler SCHED_OTHER with zero priority returns 0");
    }
/*

    // ==== sched_getparam 边界条件测试 ====
    {
        struct sched_param gp;

        // NULL pointer -> EFAULT
        CHECK_ERR(sched_getparam(0, (struct sched_param *)0x1), EFAULT, "sched_getparam with invalid pointer returns EFAULT");

        // pid 0 -> should succeed and fill gp
        memset(&gp, 0xff, sizeof(gp));
        CHECK_RET(sched_getparam(0, &gp), 0, "sched_getparam(0) returns 0");
        CHECK(gp.sched_priority >= 0, "sched_getparam(0) returns non-negative priority or zero");

        // nonexistent pid -> ESRCH
        pid_t fake = 999999;
        CHECK_ERR(sched_getparam(fake, &gp), ESRCH, "sched_getparam for non-existent pid returns ESRCH");

        // querying another pid without privileges -> EPERM (or success if root)
        if (geteuid() == 0) {
            CHECK_RET(sched_getparam(1, &gp), 0, "sched_getparam pid 1 as root returns 0");
        } else {
            CHECK_ERR(sched_getparam(1, &gp), EPERM, "sched_getparam pid 1 without privileges returns EPERM");
        }

        // negative pid -> EINVAL (unspecified by POSIX but test expected)
        CHECK_ERR(sched_getparam(-5, &gp), EINVAL, "sched_getparam with negative pid returns EINVAL");
    }

    // ==== getpriority 边界条件测试 ====
    {
        int pr;

        errno = 0;
        pr = getpriority(PRIO_PROCESS, 0);
        CHECK(errno == 0, "getpriority(PRIO_PROCESS,0) does not set errno");
        CHECK(pr >= 1 && pr <= 40, "getpriority(PRIO_PROCESS,0) returns raw priority in 1..40");

        errno = 0;
        pr = getpriority(PRIO_PGRP, 0);
        CHECK(errno == 0, "getpriority(PRIO_PGRP,0) does not set errno");
        CHECK(pr >= 1 && pr <= 40, "getpriority(PRIO_PGRP,0) returns raw priority in 1..40");

        errno = 0;
        pr = getpriority(PRIO_USER, 0);
        CHECK(errno == 0, "getpriority(PRIO_USER,0) does not set errno");
        CHECK(pr >= 1 && pr <= 40, "getpriority(PRIO_USER,0) returns raw priority in 1..40");

        // invalid which -> EINVAL
        CHECK_ERR(getpriority(0xdead, 0), EINVAL, "getpriority with invalid which returns EINVAL");

        // nonexistent pid -> ESRCH
        CHECK_ERR(getpriority(PRIO_PROCESS, 999999), ESRCH, "getpriority for non-existent pid returns ESRCH");
    }
*/

    TEST_DONE();
}