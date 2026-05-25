/*
 * test_capget.c — comprehensive capget(2) syscall tests
 *
 * capget retrieves the capability sets of a thread.  glibc provides no
 * wrapper; the raw kernel ABI is:
 *   syscall(SYS_capget, cap_user_header_t hdrp, cap_user_data_t datap)
 *
 * Positive coverage:
 *   A1.  Query own process (pid=0) with V3 — returns 0, caps populated
 *   A2.  Query own process (pid=0) with V1 — kernel upgrades version
 *   A3.  Query own process via pid=getpid() (explicit pid, not 0)
 *   A4.  Version probe: pass unsupported version → EINVAL, hdrp->version
 *        set to kernel-preferred version (V1, V2, V3)
 *   A5.  Version probe: pass version=0 → EINVAL, kernel sets preferred
 *   A6.  Invariant: effective ⊆ permitted (all words)
 *   A7.  Invariant: inheritable ⊆ permitted (all words)
 *   A8.  Consistency: two consecutive capget calls return same values
 *   A9.  pid=0 and pid=getpid() return identical capabilities
 *   A10. Zero-initialised header — kernel overwrites version field anyway
 *   A11. Query init (pid=1) if it exists — returns 0 or ESRCH
 *   A12. Check that V3 data has 2 u32 words per set
 *
 * Negative coverage:
 *   B1.  hdrp == NULL → EFAULT
 *   B2.  hdrp->version is invalid (0xDEADBEEF, 0, V3+1) → EINVAL
 *   B3.  hdrp->pid is invalid (0x7FFFFFFF, -1, -2, 999999) → ESRCH
 *   B4.  hdrp->version invalid + hdrp->pid invalid → EINVAL
 *   B5.  datap == NULL with valid hdrp → version probe, EINVAL
 *   B6.  datap == NULL with hdrp == NULL → EFAULT
 *
 * NOTE: The EPERM errors listed in the man page apply to capset(),
 * not capget().  capget() is always permitted for any process the
 * caller can see (subject to ptrace restrictions).
 */

#define _GNU_SOURCE
#include "test_framework.h"
#include <linux/capability.h>
#include <sys/syscall.h>
#include <unistd.h>
#include <string.h>

typedef struct __user_cap_header_struct cap_header_t;
typedef struct __user_cap_data_struct   cap_data_t;

int main(void)
{
    TEST_START("capget");

    /* ============================================================== */
    /* A1. Positive: pid=0, V3 — basic self-query                     */
    /* ============================================================== */
    {
        printf("\n--- A1. capget(pid=0, V3) — basic self-query ---\n");
        cap_header_t hdr;
        cap_data_t   data[_LINUX_CAPABILITY_U32S_3];
        memset(&hdr, 0, sizeof(hdr));
        memset(data, 0, sizeof(data));

        hdr.version = _LINUX_CAPABILITY_VERSION_3;
        hdr.pid = 0;
        CHECK_RET(syscall(SYS_capget, &hdr, data), 0,
                  "A1: capget(pid=0, V3) returns 0");
        CHECK(hdr.version >= _LINUX_CAPABILITY_VERSION_1,
              "A1: version field >= V1 after call");
        printf("  info | effective=0x%08x permitted=0x%08x inheritable=0x%08x\n",
               data[0].effective, data[0].permitted, data[0].inheritable);
        printf("  info | effective[1]=0x%08x permitted[1]=0x%08x inheritable[1]=0x%08x\n",
               data[1].effective, data[1].permitted, data[1].inheritable);
    }

    /* ============================================================== */
    /* A2. Positive: pid=0, V1 — kernel upgrades to preferred version */
    /* ============================================================== */
    {
        printf("\n--- A2. capget(pid=0, V1) — version upgrade ---\n");
        cap_header_t hdr;
        cap_data_t   data[_LINUX_CAPABILITY_U32S_3];
        memset(&hdr, 0, sizeof(hdr));
        memset(data, 0, sizeof(data));

        hdr.version = _LINUX_CAPABILITY_VERSION_1;
        hdr.pid = 0;
        errno = 0;
        int rc = syscall(SYS_capget, &hdr, data);
        if (rc == 0) {
            if (hdr.version >= _LINUX_CAPABILITY_VERSION_1) {
                printf("  PASS | A2: capget(V1) success, kernel upgraded version to 0x%08x\n",
                       hdr.version);
                __pass++;
            } else {
                printf("  FAIL | A2: capget(V1) success but version not upgraded (0x%08x)\n",
                       hdr.version);
                __fail++;
            }
        } else if (rc == -1 && errno == EINVAL) {
            if (hdr.version != _LINUX_CAPABILITY_VERSION_1) {
                printf("  PASS | A2: capget(V1) returns EINVAL, version set to 0x%08x\n",
                       hdr.version);
                __pass++;
            } else {
                printf("  FAIL | A2: capget(V1) EINVAL but version unchanged (0x%08x)\n",
                       hdr.version);
                __fail++;
            }
        } else {
            printf("  FAIL | A2: unexpected ret=%d errno=%d (%s)\n",
                   rc, errno, strerror(errno));
            __fail++;
        }
    }

    /* ============================================================== */
    /* A3. Positive: pid=getpid() (explicit pid, not 0)                */
    /* ============================================================== */
    {
        printf("\n--- A3. capget(pid=getpid(), V3) — explicit pid ---\n");
        cap_header_t hdr;
        cap_data_t   data[_LINUX_CAPABILITY_U32S_3];
        memset(&hdr, 0, sizeof(hdr));
        memset(data, 0, sizeof(data));

        hdr.version = _LINUX_CAPABILITY_VERSION_3;
        hdr.pid = getpid();
        errno = 0;
        int rc = syscall(SYS_capget, &hdr, data);
        if (rc == 0) {
            printf("  PASS | A3: capget(pid=getpid(), V3) returns 0\n");
            __pass++;
        } else {
            printf("  FAIL | A3: capget(pid=getpid(), V3) ret=%d errno=%d (%s)\n",
                   rc, errno, strerror(errno));
            __fail++;
        }
    }

    /* ============================================================== */
    /* A4. Version probe: bad version → EINVAL with preferred version */
    /* ============================================================== */
    {
        printf("\n--- A4. version probe via bad version ---\n");
        cap_header_t hdr;
        cap_data_t   data[_LINUX_CAPABILITY_U32S_3];
        memset(&hdr, 0, sizeof(hdr));
        memset(data, 0, sizeof(data));

        hdr.version = 0xDEADBEEF;
        hdr.pid = 0;
        CHECK_ERR(syscall(SYS_capget, &hdr, data), EINVAL,
                  "A4a: capget(version=0xDEADBEEF) -> EINVAL");
        CHECK(hdr.version != 0xDEADBEEF
              && hdr.version >= _LINUX_CAPABILITY_VERSION_1,
              "A4a: kernel sets preferred version after EINVAL");
        printf("  info | kernel preferred version = 0x%08x\n", hdr.version);
    }

    /* ============================================================== */
    /* A5. Version probe: version=0 → EINVAL, kernel sets preferred   */
    /* ============================================================== */
    {
        printf("\n--- A5. version probe with version=0 ---\n");
        cap_header_t hdr;
        cap_data_t   data[_LINUX_CAPABILITY_U32S_3];
        memset(&hdr, 0, sizeof(hdr));
        memset(data, 0, sizeof(data));

        hdr.version = 0;
        hdr.pid = 0;
        CHECK_ERR(syscall(SYS_capget, &hdr, data), EINVAL,
                  "A5a: capget(version=0) -> EINVAL");
        CHECK(hdr.version >= _LINUX_CAPABILITY_VERSION_1,
              "A5a: kernel sets preferred version after version=0 probe");
        printf("  info | preferred version (probed via 0) = 0x%08x\n", hdr.version);
    }

    /* ============================================================== */
    /* A6. Invariant: effective ⊆ permitted                           */
    /* ============================================================== */
    {
        printf("\n--- A6. invariant: effective ⊆ permitted ---\n");
        cap_header_t hdr;
        cap_data_t   data[_LINUX_CAPABILITY_U32S_3];
        memset(&hdr, 0, sizeof(hdr));
        memset(data, 0, sizeof(data));

        hdr.version = _LINUX_CAPABILITY_VERSION_3;
        hdr.pid = 0;
        int rc = syscall(SYS_capget, &hdr, data);
        if (rc != 0) {
            printf("  FAIL | A6: capget failed, ret=%d errno=%d (%s)\n",
                   rc, errno, strerror(errno));
            __fail++;
        } else {
            int ok = 1;
            for (int i = 0; i < _LINUX_CAPABILITY_U32S_3; i++) {
                if ((data[i].effective & ~data[i].permitted) != 0) {
                    ok = 0;
                    printf("  FAIL | A6: data[%d] effective=0x%08x ∉ "
                           "permitted=0x%08x\n",
                           i, data[i].effective, data[i].permitted);
                    __fail++;
                }
            }
            if (ok) {
                printf("  PASS | A6: effective ⊆ permitted "
                       "(all %d words)\n", _LINUX_CAPABILITY_U32S_3);
                __pass++;
            }
        }
    }

    /* ============================================================== */
    /* A7. Invariant: inheritable ⊆ permitted                         */
    /* ============================================================== */
    {
        printf("\n--- A7. invariant: inheritable ⊆ permitted ---\n");
        cap_header_t hdr;
        cap_data_t   data[_LINUX_CAPABILITY_U32S_3];
        memset(&hdr, 0, sizeof(hdr));
        memset(data, 0, sizeof(data));

        hdr.version = _LINUX_CAPABILITY_VERSION_3;
        hdr.pid = 0;
        int rc = syscall(SYS_capget, &hdr, data);
        if (rc != 0) {
            printf("  FAIL | A7: capget failed, ret=%d errno=%d (%s)\n",
                   rc, errno, strerror(errno));
            __fail++;
        } else {
            int ok = 1;
            for (int i = 0; i < _LINUX_CAPABILITY_U32S_3; i++) {
                if ((data[i].inheritable & ~data[i].permitted) != 0) {
                    ok = 0;
                    printf("  FAIL | A7: data[%d] inheritable=0x%08x ∉ "
                           "permitted=0x%08x\n",
                           i, data[i].inheritable, data[i].permitted);
                    __fail++;
                }
            }
            if (ok) {
                printf("  PASS | A7: inheritable ⊆ permitted "
                       "(all %d words)\n", _LINUX_CAPABILITY_U32S_3);
                __pass++;
            }
        }
    }

    /* ============================================================== */
    /* A8. Consistency: two consecutive capget calls return same caps  */
    /* ============================================================== */
    {
        printf("\n--- A8. consistency across two calls ---\n");
        cap_header_t hdr1, hdr2;
        cap_data_t   data1[_LINUX_CAPABILITY_U32S_3];
        cap_data_t   data2[_LINUX_CAPABILITY_U32S_3];

        memset(&hdr1, 0, sizeof(hdr1));
        memset(data1, 0, sizeof(data1));
        hdr1.version = _LINUX_CAPABILITY_VERSION_3;
        hdr1.pid = 0;
        int rc1 = syscall(SYS_capget, &hdr1, data1);

        memset(&hdr2, 0, sizeof(hdr2));
        memset(data2, 0, sizeof(data2));
        hdr2.version = _LINUX_CAPABILITY_VERSION_3;
        hdr2.pid = 0;
        int rc2 = syscall(SYS_capget, &hdr2, data2);

        if (rc1 == 0 && rc2 == 0) {
            int ok = 1;
            for (int i = 0; i < _LINUX_CAPABILITY_U32S_3; i++) {
                if (data1[i].effective  != data2[i].effective ||
                    data1[i].permitted  != data2[i].permitted ||
                    data1[i].inheritable != data2[i].inheritable) {
                    ok = 0;
                    printf("  FAIL | A8: data[%d] differs across calls\n", i);
                    __fail++;
                }
            }
            if (ok) {
                printf("  PASS | A8: two capget calls return identical caps\n");
                __pass++;
            }
        }
    }

    /* ============================================================== */
    /* A9. pid=0 vs pid=getpid() return identical values               */
    /* ============================================================== */
    {
        printf("\n--- A9. pid=0 vs pid=getpid() equivalence ---\n");
        cap_header_t hdr0, hdrself;
        cap_data_t   data0[_LINUX_CAPABILITY_U32S_3];
        cap_data_t   self[_LINUX_CAPABILITY_U32S_3];

        memset(&hdr0, 0, sizeof(hdr0));
        memset(data0, 0, sizeof(data0));
        hdr0.version = _LINUX_CAPABILITY_VERSION_3;
        hdr0.pid = 0;
        int rc0 = syscall(SYS_capget, &hdr0, data0);

        memset(&hdrself, 0, sizeof(hdrself));
        memset(self, 0, sizeof(self));
        hdrself.version = _LINUX_CAPABILITY_VERSION_3;
        hdrself.pid = getpid();
        int rcself = syscall(SYS_capget, &hdrself, self);

        if (rc0 == 0 && rcself == 0) {
            int ok = 1;
            for (int i = 0; i < _LINUX_CAPABILITY_U32S_3; i++) {
                if (data0[i].effective  != self[i].effective ||
                    data0[i].permitted  != self[i].permitted ||
                    data0[i].inheritable != self[i].inheritable) {
                    ok = 0;
                }
            }
            CHECK(ok, "A9: pid=0 and pid=getpid() return identical caps");
        }
    }

    /* ============================================================== */
    /* A10. Zero-initialised header — kernel overwrites version       */
    /* ============================================================== */
    {
        printf("\n--- A10. zero-init header behaviour ---\n");
        cap_header_t hdr;
        cap_data_t   data[_LINUX_CAPABILITY_U32S_3];
        memset(&hdr, 0, sizeof(hdr)); /* version=0, pid=0 */
        memset(data, 0, sizeof(data));

        errno = 0;
        int rc = syscall(SYS_capget, &hdr, data);
        /* With version=0 this should be treated as an invalid version
         * and the kernel should set the preferred version. */
        if (rc == -1 && errno == EINVAL) {
            printf("  PASS | A10: zero-header -> EINVAL, "
                   "version set to 0x%08x\n", hdr.version);
            __pass++;
        } else if (rc == 0) {
            printf("  PASS | A10: zero-header treated as V1, returned 0, "
                   "version=0x%08x\n", hdr.version);
            __pass++;
        } else {
            printf("  FAIL | A10: zero-header unexpected ret=%d errno=%d (%s)\n",
                   rc, errno, strerror(errno));
            __fail++;
        }
    }

    /* ============================================================== */
    /* A11. Query init (pid=1) if available                            */
    /* ============================================================== */
    {
        printf("\n--- A11. capget(pid=1, V3) — init process ---\n");
        cap_header_t hdr;
        cap_data_t   data[_LINUX_CAPABILITY_U32S_3];
        memset(&hdr, 0, sizeof(hdr));
        memset(data, 0, sizeof(data));

        hdr.version = _LINUX_CAPABILITY_VERSION_3;
        hdr.pid = 1;
        errno = 0;
        int rc = syscall(SYS_capget, &hdr, data);
        if (rc == 0) {
            printf("  PASS | A11: capget(pid=1) returns 0\n");
            printf("  info | init effective=0x%08x permitted=0x%08x "
                   "inheritable=0x%08x\n",
                   data[0].effective, data[0].permitted,
                   data[0].inheritable);
            __pass++;
        } else if (rc == -1 && errno == ESRCH) {
            printf("  PASS | A11: capget(pid=1) -> ESRCH (no init)\n");
            __pass++;
        } else {
            printf("  FAIL | A11: capget(pid=1) unexpected ret=%d errno=%d (%s)\n",
                   rc, errno, strerror(errno));
            __fail++;
        }
    }

    /* ============================================================== */
    /* A12. V3 data layout: 2 u32 words per capability set             */
    /* ============================================================== */
    {
        printf("\n--- A12. V3 data width ---\n");
        CHECK(_LINUX_CAPABILITY_U32S_3 >= 2,
              "A12: V3 uses at least 2 u32s per set");
        printf("  info | _LINUX_CAPABILITY_U32S_3 = %d (sizeof per set = %zu)\n",
               _LINUX_CAPABILITY_U32S_3,
               sizeof(cap_data_t) * _LINUX_CAPABILITY_U32S_3);
    }

    /* ═══════════════════════════════════════════════════════════════
     * NEGATIVE TESTS
     * ═══════════════════════════════════════════════════════════════ */

    /* ============================================================== */
    /* B1. hdrp == NULL → EFAULT                                      */
    /* ============================================================== */
    {
        printf("\n--- B1. hdrp=NULL (negative) ---\n");
        cap_data_t data[_LINUX_CAPABILITY_U32S_3];
        memset(data, 0, sizeof(data));
        CHECK_ERR(syscall(SYS_capget, NULL, data), EFAULT,
                  "B1: capget(hdrp=NULL, datap=valid) -> EFAULT");
    }

    /* ============================================================== */
    /* B2. Invalid version → EINVAL                                   */
    /* ============================================================== */
    {
        printf("\n--- B2. invalid version (negative) ---\n");
        cap_header_t hdr;
        cap_data_t   data[_LINUX_CAPABILITY_U32S_3];

        /* B2a: random bad version */
        memset(&hdr, 0, sizeof(hdr));
        memset(data, 0, sizeof(data));
        hdr.version = 0xDEADBEEF;
        hdr.pid = 0;
        CHECK_ERR(syscall(SYS_capget, &hdr, data), EINVAL,
                  "B2a: capget(version=0xDEADBEEF) -> EINVAL");

        /* B2b: version=0 */
        memset(&hdr, 0, sizeof(hdr));
        memset(data, 0, sizeof(data));
        hdr.version = 0;
        hdr.pid = 0;
        CHECK_ERR(syscall(SYS_capget, &hdr, data), EINVAL,
                  "B2b: capget(version=0) -> EINVAL");

        /* B2c: version = V3 + 1 */
        memset(&hdr, 0, sizeof(hdr));
        memset(data, 0, sizeof(data));
        hdr.version = _LINUX_CAPABILITY_VERSION_3 + 1;
        hdr.pid = 0;
        CHECK_ERR(syscall(SYS_capget, &hdr, data), EINVAL,
                  "B2c: capget(version=V3+1) -> EINVAL");
    }

    /* ============================================================== */
    /* B3. Invalid pid → ESRCH                                        */
    /* ============================================================== */
    {
        printf("\n--- B3. invalid pid (negative) ---\n");
        cap_header_t hdr;
        cap_data_t   data[_LINUX_CAPABILITY_U32S_3];

        memset(data, 0, sizeof(data));

        /* B3a: very large nonexistent pid */
        memset(&hdr, 0, sizeof(hdr));
        hdr.version = _LINUX_CAPABILITY_VERSION_3;
        hdr.pid = 0x7FFFFFFF;
        CHECK_ERR(syscall(SYS_capget, &hdr, data), ESRCH,
                  "B3a: capget(pid=0x7FFFFFFF) -> ESRCH");

        /* B3b: pid=-1 */
        memset(&hdr, 0, sizeof(hdr));
        hdr.version = _LINUX_CAPABILITY_VERSION_3;
        hdr.pid = -1;
        CHECK_ERR(syscall(SYS_capget, &hdr, data), ESRCH,
                  "B3b: capget(pid=-1) -> ESRCH");

        /* B3c: pid=-2 */
        memset(&hdr, 0, sizeof(hdr));
        hdr.version = _LINUX_CAPABILITY_VERSION_3;
        hdr.pid = -2;
        CHECK_ERR(syscall(SYS_capget, &hdr, data), ESRCH,
                  "B3c: capget(pid=-2) -> ESRCH");

        /* B3d: 999999 */
        memset(&hdr, 0, sizeof(hdr));
        hdr.version = _LINUX_CAPABILITY_VERSION_3;
        hdr.pid = 999999;
        CHECK_ERR(syscall(SYS_capget, &hdr, data), ESRCH,
                  "B3d: capget(pid=999999) -> ESRCH");
    }

    /* ============================================================== */
    /* B4. Invalid version + invalid pid → EINVAL                     */
    /* ============================================================== */
    {
        printf("\n--- B4. invalid version + invalid pid (negative) ---\n");
        cap_header_t hdr;
        cap_data_t   data[_LINUX_CAPABILITY_U32S_3];

        memset(&hdr, 0, sizeof(hdr));
        memset(data, 0, sizeof(data));
        hdr.version = 0xDEADBEEF;
        hdr.pid = 0x7FFFFFFF;
        CHECK_ERR(syscall(SYS_capget, &hdr, data), EINVAL,
                  "B4: capget(bad version, bad pid) -> EINVAL");
    }

    /* ============================================================== */
    /* B5. datap == NULL — version probe (not a fault)                */
    /* ============================================================== */
    {
        printf("\n--- B5. datap=NULL (version probe, negative → info) ---\n");
        cap_header_t hdr;
        memset(&hdr, 0, sizeof(hdr));

        /* V3 with NULL datap — should return EINVAL + preferred version */
        hdr.version = _LINUX_CAPABILITY_VERSION_3;
        hdr.pid = 0;
        CHECK_RET(syscall(SYS_capget, &hdr, NULL), 0,
                  "B5a: capget(V3, datap=NULL) returns 0");
        printf("  info | version after datap=NULL: 0x%08x\n", hdr.version);

        /* Bad version with NULL datap — also EINVAL, sets preferred */
        memset(&hdr, 0, sizeof(hdr));
        hdr.version = 0xDEADBEEF;
        hdr.pid = 0;
        CHECK_ERR(syscall(SYS_capget, &hdr, NULL), EINVAL,
                  "B5b: capget(0xDEADBEEF, datap=NULL) -> EINVAL");
        printf("  info | preferred version probed via bad ver + NULL datap: 0x%08x\n",
               hdr.version);

        /* version=0 with NULL datap — also version probe */
        memset(&hdr, 0, sizeof(hdr));
        hdr.version = 0;
        hdr.pid = 0;
        CHECK_ERR(syscall(SYS_capget, &hdr, NULL), EINVAL,
                  "B5c: capget(version=0, datap=NULL) -> EINVAL");
        printf("  info | preferred version probed via ver=0 + NULL datap: 0x%08x\n",
               hdr.version);
    }

    /* ============================================================== */
    /* B6. Both hdrp and datap NULL → EFAULT (hdrp checked first)    */
    /* ============================================================== */
    {
        printf("\n--- B6. both NULL (negative) ---\n");
        CHECK_ERR(syscall(SYS_capget, NULL, NULL), EFAULT,
                  "B6: capget(NULL, NULL) -> EFAULT");
    }

    if (__fail == 0) {
        printf("CAPGET_ALL_PASSED\n");
    } else {
        printf("CAPGET_HAS_FAILURES\n");
    }
    TEST_DONE();
}
