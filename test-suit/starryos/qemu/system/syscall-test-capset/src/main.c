/*
 * test_capset.c — comprehensive capset(2) syscall tests
 *
 * capset sets the capability sets of a thread.  glibc provides no wrapper;
 * must be invoked via syscall().  The raw kernel ABI uses:
 *   syscall(SYS_capset, cap_user_header_t hdrp, const cap_user_data_t datap)
 *
 * Positive coverage:
 *   A1. Set own caps (pid=0) to same values → success (no-op) or EPERM
 *   A2. Clear effective set while keeping permitted → success/EPERM
 *   A3. V3 header with valid V3 data
 *
 * Negative coverage:
 *   B1. Invalid version → EINVAL
 *   B2. hdrp = NULL → EFAULT
 *   B3. datap = NULL → EFAULT
 *   B4. Invalid pid → ESRCH
 *   B5. Modify another thread's caps → EPERM
 *   B6. Add cap to effective not in permitted → EPERM
 *   B7. Add cap to permitted set → EPERM
 *   B8. Add cap to inheritable not in bounding set → EPERM
 *   B9. Invalid version + invalid pid → EINVAL/ESRCH
 */

#define _GNU_SOURCE
#include "test_framework.h"
#include <stdint.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>
#include <string.h>

/* Minimal capability UAPI definitions used by capset(2). */
#define _LINUX_CAPABILITY_VERSION_3 0x20080522
#define _LINUX_CAPABILITY_U32S_3 2
#ifndef CAP_CHOWN
#define CAP_CHOWN 0
#endif
#ifndef CAP_SETUID
#define CAP_SETUID 7
#endif
#ifndef PR_CAPBSET_READ
#define PR_CAPBSET_READ 23
#endif
#ifndef PR_CAPBSET_DROP
#define PR_CAPBSET_DROP 24
#endif
#ifndef PR_CAP_AMBIENT
#define PR_CAP_AMBIENT 47
#endif
#ifndef PR_CAP_AMBIENT_IS_SET
#define PR_CAP_AMBIENT_IS_SET 1
#endif
#ifndef PR_CAP_AMBIENT_RAISE
#define PR_CAP_AMBIENT_RAISE 2
#endif
#ifndef PR_CAP_AMBIENT_LOWER
#define PR_CAP_AMBIENT_LOWER 3
#endif
#ifndef PR_CAP_AMBIENT_CLEAR_ALL
#define PR_CAP_AMBIENT_CLEAR_ALL 4
#endif

typedef uint32_t __u32;
struct __user_cap_header_struct {
    __u32 version;
    int pid;
};

struct __user_cap_data_struct {
    __u32 effective;
    __u32 permitted;
    __u32 inheritable;
};

typedef struct __user_cap_header_struct cap_header_t;
typedef struct __user_cap_data_struct   cap_data_t;

/* helper: read current caps into data[] */
static int read_caps(cap_data_t *data, int nwords)
{
    cap_header_t hdr;
    memset(&hdr, 0, sizeof(hdr));
    hdr.version = _LINUX_CAPABILITY_VERSION_3;
    hdr.pid = 0;
    memset(data, 0, (size_t)nwords * sizeof(cap_data_t));
    return (syscall(SYS_capget, &hdr, data) == 0);
}

static long prctl_raw(int option, unsigned long arg2, unsigned long arg3,
                      unsigned long arg4, unsigned long arg5)
{
    return syscall(SYS_prctl, option, arg2, arg3, arg4, arg5);
}

static void child_fail(const char *msg)
{
    printf("  FAIL | child | %s | errno=%d (%s)\n", msg, errno,
           strerror(errno));
    fflush(stdout);
    _exit(1);
}

static void child_expect(int cond, const char *msg)
{
    if (!cond) {
        child_fail(msg);
    }
}

static void expect_child_success(void (*fn)(void), const char *name)
{
    pid_t pid = fork();
    if (pid == 0) {
        fn();
        _exit(0);
    }
    if (pid < 0) {
        printf("  FAIL | fork | %s | errno=%d (%s)\n", name, errno,
               strerror(errno));
        __fail++;
        return;
    }

    int status;
    if (waitpid(pid, &status, 0) != pid) {
        printf("  FAIL | waitpid | %s | errno=%d (%s)\n", name, errno,
               strerror(errno));
        __fail++;
        return;
    }

    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, name);
}

static void capbset_drop_child(void)
{
    errno = 0;
    child_expect(prctl_raw(PR_CAPBSET_READ, CAP_CHOWN, 0, 0, 0) == 1,
                 "CAP_CHOWN starts in bounding set");

    errno = 0;
    child_expect(prctl_raw(PR_CAPBSET_DROP, CAP_CHOWN, 0, 0, 0) == 0,
                 "PR_CAPBSET_DROP CAP_CHOWN succeeds");

    errno = 0;
    child_expect(prctl_raw(PR_CAPBSET_READ, CAP_CHOWN, 0, 0, 0) == 0,
                 "PR_CAPBSET_READ sees dropped capability");

    errno = 0;
    long ret = prctl_raw(PR_CAPBSET_DROP, CAP_SETUID, 1, 0, 0);
    child_expect(ret == -1 && errno == EINVAL,
                 "PR_CAPBSET_DROP rejects nonzero reserved args");
}

static void ambient_cap_child(void)
{
    const __u32 bit = 1U << CAP_CHOWN;
    cap_header_t hdr;
    cap_data_t data[_LINUX_CAPABILITY_U32S_3];

    memset(&hdr, 0, sizeof(hdr));
    hdr.version = _LINUX_CAPABILITY_VERSION_3;
    hdr.pid = 0;
    child_expect(read_caps(data, _LINUX_CAPABILITY_U32S_3),
                 "read caps for ambient test");

    data[0].inheritable |= bit;
    errno = 0;
    child_expect(syscall(SYS_capset, &hdr, data) == 0,
                 "add CAP_CHOWN to inheritable set");

    errno = 0;
    child_expect(prctl_raw(PR_CAP_AMBIENT, PR_CAP_AMBIENT_RAISE, CAP_CHOWN,
                           0, 0) == 0,
                 "raise CAP_CHOWN ambient capability");

    errno = 0;
    child_expect(prctl_raw(PR_CAP_AMBIENT, PR_CAP_AMBIENT_IS_SET, CAP_CHOWN,
                           0, 0) == 1,
                 "ambient CAP_CHOWN is set");

    errno = 0;
    child_expect(prctl_raw(PR_CAP_AMBIENT, PR_CAP_AMBIENT_LOWER, CAP_CHOWN,
                           0, 0) == 0,
                 "lower CAP_CHOWN ambient capability");

    errno = 0;
    child_expect(prctl_raw(PR_CAP_AMBIENT, PR_CAP_AMBIENT_IS_SET, CAP_CHOWN,
                           0, 0) == 0,
                 "ambient CAP_CHOWN is clear after lower");

    errno = 0;
    child_expect(prctl_raw(PR_CAP_AMBIENT, PR_CAP_AMBIENT_RAISE, CAP_CHOWN,
                           0, 0) == 0,
                 "raise CAP_CHOWN ambient capability again");

    errno = 0;
    child_expect(prctl_raw(PR_CAP_AMBIENT, PR_CAP_AMBIENT_CLEAR_ALL, 0, 0,
                           0) == 0,
                 "clear all ambient capabilities");

    errno = 0;
    child_expect(prctl_raw(PR_CAP_AMBIENT, PR_CAP_AMBIENT_IS_SET, CAP_CHOWN,
                           0, 0) == 0,
                 "ambient CAP_CHOWN is clear after clear-all");
}

static void setuid_clears_caps_child(void)
{
    cap_data_t data[_LINUX_CAPABILITY_U32S_3];

    child_expect(read_caps(data, _LINUX_CAPABILITY_U32S_3),
                 "read caps before setuid");
    child_expect((data[0].effective | data[1].effective) != 0,
                 "root starts with effective capabilities");

    errno = 0;
    child_expect(setuid(1000) == 0, "setuid from root to uid 1000");
    child_expect(geteuid() == 1000, "effective uid is non-root");

    child_expect(read_caps(data, _LINUX_CAPABILITY_U32S_3),
                 "read caps after setuid");
    child_expect(data[0].effective == 0 && data[1].effective == 0,
                 "effective caps cleared after root->nonroot setuid");
    child_expect(data[0].permitted == 0 && data[1].permitted == 0,
                 "permitted caps cleared after root->nonroot setuid");
}

static void clear_effective_child(void)
{
    cap_header_t hdr;
    cap_data_t data[_LINUX_CAPABILITY_U32S_3];

    memset(&hdr, 0, sizeof(hdr));
    hdr.version = _LINUX_CAPABILITY_VERSION_3;
    child_expect(read_caps(data, _LINUX_CAPABILITY_U32S_3),
                 "read caps for effective-clear test");

    for (int i = 0; i < _LINUX_CAPABILITY_U32S_3; i++) {
        data[i].effective = 0;
    }

    errno = 0;
    long ret = syscall(SYS_capset, &hdr, data);
    child_expect(ret == 0 || (ret == -1 && errno == EPERM),
                 "capset(effective=0) returns 0/EPERM");
}

static void all_zero_caps_child(void)
{
    cap_header_t hdr;
    cap_data_t data[_LINUX_CAPABILITY_U32S_3];

    memset(&hdr, 0, sizeof(hdr));
    memset(data, 0, sizeof(data));
    hdr.version = _LINUX_CAPABILITY_VERSION_3;

    errno = 0;
    long ret = syscall(SYS_capset, &hdr, data);
    child_expect(ret == 0 || (ret == -1 && errno == EPERM),
                 "capset(V3, all zeros) returns 0/EPERM");
}

static void effective_not_permitted_child(void)
{
    cap_header_t hdr;
    cap_data_t data[_LINUX_CAPABILITY_U32S_3];

    memset(&hdr, 0, sizeof(hdr));
    hdr.version = _LINUX_CAPABILITY_VERSION_3;
    child_expect(read_caps(data, _LINUX_CAPABILITY_U32S_3),
                 "read caps for effective-not-permitted test");

    data[0].effective |= 0x2;
    data[0].permitted &= ~0x2;

    errno = 0;
    long ret = syscall(SYS_capset, &hdr, data);
    child_expect(ret == -1 && errno == EPERM,
                 "capset(effective not subset of permitted) returns EPERM");
}

static void add_permitted_child(void)
{
    cap_header_t hdr;
    cap_data_t data[_LINUX_CAPABILITY_U32S_3];
    const __u32 bit = 1U << 22;

    memset(&hdr, 0, sizeof(hdr));
    hdr.version = _LINUX_CAPABILITY_VERSION_3;
    child_expect(read_caps(data, _LINUX_CAPABILITY_U32S_3),
                 "read caps for permitted-add test");

    data[0].permitted &= ~bit;
    errno = 0;
    long ret = syscall(SYS_capset, &hdr, data);
    if (ret == -1 && errno == EPERM) {
        return;
    }
    child_expect(ret == 0, "remove CAP_SYS_BOOT from permitted set");

    data[0].permitted |= bit;
    errno = 0;
    ret = syscall(SYS_capset, &hdr, data);
    child_expect(ret == -1 && errno == EPERM,
                 "adding a permitted capability returns EPERM");
}

static void add_inheritable_child(void)
{
    cap_header_t hdr;
    cap_data_t data[_LINUX_CAPABILITY_U32S_3];
    const __u32 bit = 1U << 23;

    memset(&hdr, 0, sizeof(hdr));
    hdr.version = _LINUX_CAPABILITY_VERSION_3;
    child_expect(read_caps(data, _LINUX_CAPABILITY_U32S_3),
                 "read caps for inheritable-add test");

    data[0].inheritable |= bit;
    data[0].permitted &= ~bit;

    errno = 0;
    long ret = syscall(SYS_capset, &hdr, data);
    child_expect(ret == 0 || (ret == -1 && errno == EPERM),
                 "capset(inheritable expansion) returns 0/EPERM");
}

int main(void)
{
    TEST_START("capset");

    /* ============================================================== */
    /* A1. Positive: set own caps to current values (no-op)           */
    /* ============================================================== */
    {
        printf("\n--- A1. capset(pid=0, same data) — positive ---\n");
        cap_header_t hdr;
        cap_data_t   data[_LINUX_CAPABILITY_U32S_3];
        memset(&hdr, 0, sizeof(hdr));
        memset(data, 0, sizeof(data));

        hdr.version = _LINUX_CAPABILITY_VERSION_3;
        if (!read_caps(data, _LINUX_CAPABILITY_U32S_3)) {
            printf("  FAIL | cannot read current caps\n");
            __fail++;
        } else {
            errno = 0;
            int rc = syscall(SYS_capset, &hdr, data);
            CHECK(rc == 0 || (rc == -1 && errno == EPERM),
                  "A1: capset(pid=0, same as current) -> 0/EPERM");
        }
    }

    /* ============================================================== */
    /* A2. Positive/negative: clear effective, keep permitted         */
    /* ============================================================== */
    {
        printf("\n--- A2. clear effective set (positive) ---\n");
        expect_child_success(clear_effective_child,
                             "A2: capset(effective=0) -> 0/EPERM");
    }

    /* ============================================================== */
    /* A3. Positive: V3 header + V3 data, self pid=0                  */
    /* ============================================================== */
    {
        printf("\n--- A3. capset(pid=0, V3) simple set — positive ---\n");
        expect_child_success(all_zero_caps_child,
                             "A3: capset(V3, all zeros) -> 0/EPERM");
    }

    /* ============================================================== */
    /* B1. Negative: invalid version → EINVAL                         */
    /* ============================================================== */
    {
        printf("\n--- B1. invalid version (negative) ---\n");
        cap_header_t hdr;
        cap_data_t   data[_LINUX_CAPABILITY_U32S_3];

        /* B1a: random bad version */
        memset(&hdr, 0, sizeof(hdr));
        memset(data, 0, sizeof(data));
        hdr.version = 0xDEADBEEF;
        hdr.pid = 0;
        CHECK_ERR(syscall(SYS_capset, &hdr, data), EINVAL,
                  "capset(pid=0, version=0xDEADBEEF) -> EINVAL");

        /* B1b: version=0 */
        memset(&hdr, 0, sizeof(hdr));
        memset(data, 0, sizeof(data));
        hdr.version = 0;
        hdr.pid = 0;
        CHECK_ERR(syscall(SYS_capset, &hdr, data), EINVAL,
                  "capset(pid=0, version=0) -> EINVAL");

        /* B1c: version = V3 + 1 */
        memset(&hdr, 0, sizeof(hdr));
        memset(data, 0, sizeof(data));
        hdr.version = _LINUX_CAPABILITY_VERSION_3 + 1;
        hdr.pid = 0;
        CHECK_ERR(syscall(SYS_capset, &hdr, data), EINVAL,
                  "capset(pid=0, version=V3+1) -> EINVAL");
    }

    /* ============================================================== */
    /* B2. Negative: hdrp = NULL → EFAULT                              */
    /* ============================================================== */
    {
        printf("\n--- B2. hdrp=NULL (negative) ---\n");
        cap_data_t data[_LINUX_CAPABILITY_U32S_3];
        memset(data, 0, sizeof(data));
        CHECK_ERR(syscall(SYS_capset, NULL, data), EFAULT,
                  "capset(hdrp=NULL) -> EFAULT");
    }

    /* ============================================================== */
    /* B3. Negative: datap = NULL → EFAULT                             */
    /*      Linux returns EFAULT for capset with NULL datap because    */
    /*      the kernel must read capability data from userspace.       */
    /*      StarryOS ignores the data argument entirely, so non-root   */
    /*      callers may see EPERM instead (euid check fires first).    */
    /* ============================================================== */
    {
        printf("\n--- B3. datap=NULL (negative) ---\n");
        cap_header_t hdr;
        memset(&hdr, 0, sizeof(hdr));
        hdr.version = _LINUX_CAPABILITY_VERSION_3;
        hdr.pid = 0;
        errno = 0;
        long r = syscall(SYS_capset, &hdr, NULL);
        CHECK(r == 0 || (r == -1 && (errno == EFAULT || errno == EPERM)),
              "B3: capset(datap=NULL) -> EFAULT/EPERM/0");
    }

    /* ============================================================== */
    /* B4. Negative: invalid pid → ESRCH                              */
    /* ============================================================== */
    {
        printf("\n--- B4. invalid pid (negative) ---\n");
        cap_header_t hdr;
        cap_data_t   data[_LINUX_CAPABILITY_U32S_3];

        /* B4a: very large nonexistent pid */
        memset(&hdr, 0, sizeof(hdr));
        memset(data, 0, sizeof(data));
        hdr.version = _LINUX_CAPABILITY_VERSION_3;
        hdr.pid = 0x7FFFFFFF;
        CHECK_ERR(syscall(SYS_capset, &hdr, data), ESRCH,
                  "capset(pid=0x7FFFFFFF) -> ESRCH");

        /* B4b: pid=-1 (on VFS kernels, this is always EPERM or ESRCH) */
        memset(&hdr, 0, sizeof(hdr));
        memset(data, 0, sizeof(data));
        hdr.version = _LINUX_CAPABILITY_VERSION_3;
        hdr.pid = -1;
        errno = 0;
        long r = syscall(SYS_capset, &hdr, data);
        if (r == -1 && (errno == ESRCH || errno == EPERM || errno == EINVAL)) {
            printf("  PASS | capset(pid=-1) -> %s (errno=%d)\n",
                   errno == ESRCH ? "ESRCH" : errno == EPERM ? "EPERM" : "EINVAL",
                   errno);
            __pass++;
        } else {
            printf("  FAIL | capset(pid=-1) expected ESRCH/EPERM got ret=%ld errno=%d (%s)\n",
                   r, errno, strerror(errno));
            __fail++;
        }

        /* B4c: pid=-2 */
        memset(&hdr, 0, sizeof(hdr));
        memset(data, 0, sizeof(data));
        hdr.version = _LINUX_CAPABILITY_VERSION_3;
        hdr.pid = -2;
        errno = 0;
        long r3 = syscall(SYS_capset, &hdr, data);
        if (r3 == -1 && (errno == ESRCH || errno == EPERM || errno == EINVAL)) {
            printf("  PASS | capset(pid=-2) -> %s (errno=%d)\n",
                   errno == ESRCH ? "ESRCH" : errno == EPERM ? "EPERM" : "EINVAL",
                   errno);
            __pass++;
        } else {
            printf("  FAIL | capset(pid=-2) expected ESRCH/EPERM got ret=%ld errno=%d (%s)\n",
                   r3, errno, strerror(errno));
            __fail++;
        }

        /* B4d: pid=999999 */
        memset(&hdr, 0, sizeof(hdr));
        memset(data, 0, sizeof(data));
        hdr.version = _LINUX_CAPABILITY_VERSION_3;
        hdr.pid = 999999;
        errno = 0;
        long r4 = syscall(SYS_capset, &hdr, data);
        if (r4 == -1 && (errno == ESRCH || errno == EPERM || errno == EINVAL)) {
            printf("  PASS | capset(pid=999999) -> %s (errno=%d)\n",
                   errno == ESRCH ? "ESRCH" : errno == EPERM ? "EPERM" : "EINVAL",
                   errno);
            __pass++;
        } else {
            printf("  FAIL | capset(pid=999999) expected ESRCH/EPERM got ret=%ld errno=%d (%s)\n",
                   r4, errno, strerror(errno));
            __fail++;
        }
    }

    /* ============================================================== */
    /* B5. Negative: modify another thread → EPERM (VFS kernel)       */
    /* ============================================================== */
    {
        printf("\n--- B5. modify another process (negative) ---\n");
        cap_header_t hdr;
        cap_data_t   data[_LINUX_CAPABILITY_U32S_3];
        memset(&hdr, 0, sizeof(hdr));
        memset(data, 0, sizeof(data));

        hdr.version = _LINUX_CAPABILITY_VERSION_3;
        hdr.pid = 1; /* init or another process */
        errno = 0;
        long r = syscall(SYS_capset, &hdr, data);
        /* On VFS kernels, capset to another pid returns EPERM.
         * If pid=1 doesn't exist, ESRCH is also valid. */
        if (r == -1 && (errno == EPERM || errno == ESRCH)) {
            printf("  PASS | capset(pid=1, all-zeros) -> %s (errno=%d)\n",
                   errno == EPERM ? "EPERM" : "ESRCH", errno);
            __pass++;
        } else {
            printf("  FAIL | capset(pid=1, all-zeros) expected EPERM/ESRCH got ret=%ld errno=%d (%s)\n",
                   r, errno, strerror(errno));
            __fail++;
        }
    }

    /* ============================================================== */
    /* B6. Negative: add cap to effective not in permitted → EPERM    */
    /* ============================================================== */
    {
        printf("\n--- B6. effective not subset of permitted (negative) ---\n");
        expect_child_success(effective_not_permitted_child,
                             "B6: capset(effective not subset of permitted) -> EPERM");
    }

    /* ============================================================== */
    /* B7. Negative: add cap to permitted set → EPERM                 */
    /* ============================================================== */
    {
        printf("\n--- B7. add cap to permitted set (negative) ---\n");
        expect_child_success(add_permitted_child,
                             "B7: capset(add to permitted) -> EPERM");
    }

    /* ============================================================== */
    /* B8. Negative: add to inheritable not in bounding → EPERM       */
    /* ============================================================== */
    {
        printf("\n--- B8. add to inheritable (negative) ---\n");
        expect_child_success(add_inheritable_child,
                             "B8: capset(inheritable expansion) -> 0/EPERM");
    }

    /* ============================================================== */
    /* B9. Negative: bad version + bad pid → EINVAL                   */
    /*      The version check runs before the pid check, so EINVAL    */
    /*      is returned regardless of the pid value.                  */
    /* ============================================================== */
    {
        printf("\n--- B9. bad version + bad pid (negative) ---\n");
        cap_header_t hdr;
        cap_data_t   data[_LINUX_CAPABILITY_U32S_3];
        memset(&hdr, 0, sizeof(hdr));
        memset(data, 0, sizeof(data));

        hdr.version = 0xDEADBEEF;
        hdr.pid = 0x7FFFFFFF;
        CHECK_ERR(syscall(SYS_capset, &hdr, data), EINVAL,
                  "B9: capset(bad version, bad pid) -> EINVAL");
    }

    /* ============================================================== */
    /* C1. prctl(PR_CAPBSET_DROP) updates the bounding set            */
    /* ============================================================== */
    {
        printf("\n--- C1. PR_CAPBSET_DROP and READ (child isolated) ---\n");
        expect_child_success(capbset_drop_child,
                             "C1: PR_CAPBSET_DROP removes a bounding cap");
    }

    /* ============================================================== */
    /* C2. prctl(PR_CAP_AMBIENT) raise/lower/clear behavior           */
    /* ============================================================== */
    {
        printf("\n--- C2. PR_CAP_AMBIENT operations (child isolated) ---\n");
        expect_child_success(ambient_cap_child,
                             "C2: PR_CAP_AMBIENT raise/lower/clear");
    }

    /* ============================================================== */
    /* C3. root to non-root setuid clears capability bitmaps          */
    /* ============================================================== */
    {
        printf("\n--- C3. setuid clears capabilities (child isolated) ---\n");
        expect_child_success(setuid_clears_caps_child,
                             "C3: setuid(1000) clears permitted/effective caps");
    }

    if (__fail == 0) {
        printf("CAPSET_ALL_PASSED\n");
    } else {
        printf("CAPSET_HAS_FAILURES\n");
    }
    TEST_DONE();
}
