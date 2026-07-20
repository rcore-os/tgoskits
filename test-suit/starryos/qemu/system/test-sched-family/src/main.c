#define _GNU_SOURCE
#include "test_framework.h"
#include <sched.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <unistd.h>
#include <errno.h>
#include <string.h>
#include <signal.h>


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

static long fill_online_cpu_mask(cpu_set_t *mask)
{
    long nprocs = sysconf(_SC_NPROCESSORS_ONLN);
    if (nprocs < 1)
        nprocs = 1;

    CPU_ZERO(mask);
    for (long cpu = 0; cpu < nprocs && cpu < CPU_SETSIZE; cpu++) {
        CPU_SET((int)cpu, mask);
    }
    return nprocs;
}

static int contains_online_cpus(const cpu_set_t *mask, long nprocs)
{
    for (long cpu = 0; cpu < nprocs && cpu < CPU_SETSIZE; cpu++) {
        if (!CPU_ISSET((int)cpu, mask))
            return 0;
    }
    return 1;
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

        // 常规用例：将当前线程绑定到所有在线 CPU，并验证 getaffinity 返回正确的结果
        {
            cpu_set_t mask, readback;
            long nprocs = fill_online_cpu_mask(&mask);

            memset(&readback, 0, sizeof(readback));
            CHECK_RET(sched_setaffinity(0, sizeof(mask), &mask), 0, "setaffinity current pid to online CPUs");
            CHECK_RET(sched_getaffinity(0, sizeof(readback), &readback), 0, "getaffinity current pid");
            CHECK(contains_online_cpus(&readback, nprocs), "getaffinity result contains online CPUs");
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

        // ==== EPERM: sched_setaffinity permission test (fork-based) ====
        {
            pid_t target = fork();
            if (target == 0) {
                while (1) pause(); // child stays alive
            }

            cpu_set_t mask;
            CPU_ZERO(&mask);
            CPU_SET(0, &mask);

            uid_t my_euid = geteuid();

            // fork child: uid/euid == parent (no /proc parsing needed)
            uid_t target_ruid = my_euid;
            uid_t target_euid = my_euid;

            if (my_euid == target_ruid || my_euid == target_euid || has_cap_sys_nice()) {
                CHECK_RET(sched_setaffinity(target, sizeof(mask), &mask), 0,
                    "setaffinity forked child allowed returns 0");
            } else {
                CHECK_ERR(sched_setaffinity(target, sizeof(mask), &mask), EPERM,
                    "setaffinity forked child returns EPERM");
            }

            kill(target, SIGKILL);
            waitpid(target, NULL, 0);
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

    // 测试 sched_setscheduler() / sched_getscheduler 
    {
        #define SCHED_RESET_ON_FORK 0x40000000
        struct sched_param sp;
        memset(&sp, 0, sizeof(sp));
        sp.sched_priority = 0;
        int policy = SCHED_FIFO | SCHED_RESET_ON_FORK;

        // 正常情况测试
        CHECK_RET(syscall(SYS_SCHED_SETSCHEDULER, 0, SCHED_OTHER, &sp), 0, "sched_setscheduler with valid parameters returns 0");
        CHECK_RET(syscall(SYS_SCHED_GETSCHEDULER, 0), SCHED_OTHER, "sched_getscheduler for current pid returns SCHED_OTHER");

        // 负 pid -> EINVAL
        CHECK_ERR(syscall(SYS_SCHED_SETSCHEDULER, -5, SCHED_OTHER, &sp), EINVAL, "sched_setscheduler with negative pid returns EINVAL");

        CHECK_ERR(syscall(SYS_SCHED_GETSCHEDULER, -5), EINVAL, "sched_getscheduler with negative pid returns EINVAL");

        // NULL param -> EINVAL
        CHECK_ERR(syscall(SYS_SCHED_SETSCHEDULER, 0, SCHED_OTHER, NULL), EINVAL, "sched_setscheduler with NULL param returns EINVAL");

        // invalid policy
        CHECK_ERR(syscall(SYS_SCHED_SETSCHEDULER, 0, 0xdeadbeef, &sp), EINVAL, "sched_setscheduler with invalid policy returns EINVAL");

        // FIFO | RESET_ON_FORK is valid combination
        sp.sched_priority = 2;
        CHECK_RET(syscall(SYS_SCHED_SETSCHEDULER, 0, policy, &sp), 0, "sched_setscheduler with SCHED_RESET_ON_FORK should succeed");

        // SCHED_OTHER with non-zero priority
        sp.sched_priority = 1;
        CHECK_ERR(syscall(SYS_SCHED_SETSCHEDULER, 0, SCHED_OTHER, &sp), EINVAL, "sched_setscheduler SCHED_OTHER with nonzero priority returns EINVAL");
        sp.sched_priority = 0;

        // SCHED_BATCH with non-zero priority
        sp.sched_priority = 1;
        CHECK_ERR(syscall(SYS_SCHED_SETSCHEDULER, 0, SCHED_BATCH, &sp), EINVAL, "sched_setscheduler SCHED_BATCH with nonzero priority returns EINVAL");
        sp.sched_priority = 0;

        // SCHED_IDLE with non-zero priority
        sp.sched_priority = 1;
        CHECK_ERR(syscall(SYS_SCHED_SETSCHEDULER, 0, SCHED_IDLE, &sp), EINVAL, "sched_setscheduler SCHED_IDLE with nonzero priority returns EINVAL");
        sp.sched_priority = 0;

        // sched_getscheduler(0)
        CHECK(syscall(SYS_SCHED_GETSCHEDULER, 0) >= 0, "sched_getscheduler(0) returns non-negative policy");

        // fake pid -> ESRCH
        CHECK_ERR(syscall(SYS_SCHED_GETSCHEDULER, 999999), ESRCH, "sched_getscheduler for non-existent pid returns ESRCH");
        CHECK_ERR(syscall(SYS_SCHED_SETSCHEDULER, 999999, SCHED_OTHER, &sp), ESRCH, "sched_setscheduler for non-existent pid returns ESRCH");

        // ==== EPERM: sched_setscheduler permission test (fork-based) ====
        {
            pid_t target = fork();
            if (target == 0) {
                while (1) pause(); // child stays alive
            }

            cpu_set_t mask;
            CPU_ZERO(&mask);
            CPU_SET(0, &mask);

            uid_t my_euid = geteuid();

            // fork child: uid/euid == parent (no /proc parsing needed)
            uid_t target_ruid = my_euid;
            uid_t target_euid = my_euid;

            if (my_euid == target_ruid || my_euid == target_euid || has_cap_sys_nice()) {
                CHECK_RET(syscall(SYS_SCHED_SETSCHEDULER, target, SCHED_OTHER, &sp), 0,
                    "sched_setscheduler forked child allowed returns 0");
            } else {
                CHECK_ERR(syscall(SYS_SCHED_SETSCHEDULER, target, SCHED_OTHER, &sp), EPERM,
                    "sched_setscheduler forked child returns EPERM");
            }

            kill(target, SIGKILL);
            waitpid(target, NULL, 0);
        }
    }

    // 测试 sched_getparam() 边界条件和权限
    {
        struct sched_param gp;
        memset(&gp, 0, sizeof(gp));

        int pol, prio;
        CHECK_RET(syscall(SYS_SCHED_GETPARAM, 0, &gp), 0, "sched_getparam for current pid returns 0");

        pol = syscall(SYS_SCHED_GETSCHEDULER, 0);
        prio = gp.sched_priority;

        if (pol == SCHED_FIFO || pol == SCHED_RR) {
            CHECK(prio >= 1 && prio <= 99, "RT priority range");
        } else {
            CHECK(prio == 0, "non-RT priority must be 0");
        }

        // fake pid -> ESRCH
        CHECK_ERR(syscall(SYS_SCHED_GETPARAM, 999999, &gp), ESRCH, "sched_getparam for non-existent pid returns ESRCH");
    }

    // ==== getpriority 边界条件测试 ====
    {
        int pr;

        errno = 0;
        pr = getpriority(PRIO_PROCESS, 0);
        CHECK(errno == 0, "getpriority(PRIO_PROCESS,0) does not set errno");
        CHECK(pr >= -20 && pr <= 19, "getpriority(PRIO_PROCESS,0) returns raw priority in -20..19");

        errno = 0;
        pr = getpriority(PRIO_PGRP, 0);
        CHECK(errno == 0, "getpriority(PRIO_PGRP,0) does not set errno");
        CHECK(pr >= -20 && pr <= 19, "getpriority(PRIO_PGRP,0) returns raw priority in -20..19");

        errno = 0;
        pr = getpriority(PRIO_USER, 0);
        CHECK(errno == 0, "getpriority(PRIO_USER,0) does not set errno");
        CHECK(pr >= -20 && pr <= 19, "getpriority(PRIO_USER,0) returns raw priority in -20..19");

        // invalid which -> EINVAL
        CHECK_ERR(getpriority(0xdead, 0), EINVAL, "getpriority with invalid which returns EINVAL");

        // nonexistent pid -> ESRCH
        CHECK_ERR(getpriority(PRIO_PROCESS, 999999), ESRCH, "getpriority for non-existent pid returns ESRCH");
    }

    TEST_DONE();
}
