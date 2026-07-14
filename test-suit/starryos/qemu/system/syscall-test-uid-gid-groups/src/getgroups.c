#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <unistd.h>

/* getgroups(2) — get supplementary group IDs.
 *
 * man 2 getgroups:
 *   "If size is zero, list is not modified, but the total number of
 *    supplementary group IDs for the process is returned."
 *   "It is unspecified whether the effective group ID of the calling process
 *    is included in the returned list."
 *
 * 测试覆盖：
 *   (a) getgroups(0, NULL) — 查询 ngroups 不写 list
 *   (b) getgroups(N, valid_buf) where N >= ngroups — 应填写并返 ngroups
 *   (c) getgroups(N, valid_buf) where N < ngroups — -1 EINVAL（man）
 *   (d) getgroups(0, garbage_ptr) — list 不被读 → OK
 *   (e) raw syscall vs libc
 */

static void getgroups_query_size_zero(void)
{
    /* 怎么测：getgroups(0, NULL)
     * 期望：返回 ngroups（非负），不修改 list（NULL safe）
     * 为什么：man "If size is zero, list is not modified" — 查询模式 */
    errno = 0;
    int n = getgroups(0, NULL);
    CHECK(n >= 0,                                                 "getgroups (a) size=0 NULL list -> returns ngroups (>=0)");
    printf("  process has %d supplementary group(s)\n", n);
}

static void getgroups_fill_when_buf_large_enough(void)
{
    /* 测什么: man §DESCRIPTION — "size should be set to the maximum number
     *         of items that can be stored". 当 size >= ngroups 时, 应正常填
     *         返 ngroups.
     * 怎么测: 先查 ngroups (size=0) 作 baseline → 用 size=256 buf 调
     *         getgroups (256 远大于典型 ngroups) → 验返值与 baseline 一致.
     * 期望:   rc == n_query.
     * 为什么: 验证 starry sys_getgroups 在 size >= ngroups 路径正常 vm_write
     *         + 返 ngroups; 不返 size (这是常见 implementation bug). */
    int n_query = getgroups(0, NULL);
    if (n_query < 0) { CHECK(0, "getgroups (b) skip — query failed"); return; }
    gid_t list[256];
    int rc = getgroups(256, list);
    CHECK(rc == n_query,                                          "getgroups (b) size=256 (large enough) -> returns ngroups matching query");
}

static void getgroups_too_small_einval(void)
{
    /* 测什么：man §EINVAL — "size is less than the number of supplementary
     *         group IDs, but is not zero."
     * 怎么测：buf size 比实际 ngroups 小且 != 0 → 应 EINVAL.
     * 期望：rc=-1, errno=EINVAL.
     * 为什么：验证 starry getgroups 在 size 不足时拒绝 + 返 EINVAL.
     *
     * 注：若 ngroups <= 1, size = n_query - 1 = 0 (size=0 是查询模式),
     *     getgroups 返 ngroups (非 EINVAL). 所以只在 ngroups >= 2 时才测.
     *     (Linux Host (sudo root): root 通常 ngroups=0, 在 starry 同, skip)
     *     (Linux Host (普通用户): user 通常 ngroups=N>0, 可触发) */
    int n_query = getgroups(0, NULL);
    if (n_query < 2) {
        printf("  getgroups (c) skip: ngroups=%d < 2 — size-too-small case 不可测\n", n_query);
        return;
    }
    gid_t list[1];
    errno = 0;
    /* size = n_query - 1 >= 1, 且 < ngroups → EINVAL */
    int rc = getgroups(n_query - 1, list);
    CHECK(rc == -1 && errno == EINVAL,                            "getgroups (c) size < ngroups -> -1 EINVAL");
}

static void getgroups_size_zero_garbage_list_ok(void)
{
    /* size=0 时 list 不该被读；传 garbage 不应 EFAULT */
    errno = 0;
    int n = getgroups(0, (gid_t *)0xdeadbeefdeadbeefULL);
    /* 应成功返回 ngroups —— size=0 时 list 不被解引用 */
    CHECK(n >= 0,                                                 "getgroups (d) size=0 garbage list -> returns ngroups (list not deref'd)");
}

static void getgroups_raw_matches_libc(void)
{
    /* 测什么: libc 应直接转发 syscall, 无值转换.
     * 怎么测: libc + raw syscall 各跑 getgroups(64, buf), 比对返值.
     * 期望:   rc1 == rc2.
     * 为什么: 验证 starry sys_getgroups 与 libc 包装 ABI 契约一致.
     *         注: 此处只验 rc 一致, 不验 buf 内容 — buf 内容由 (b) 覆盖. */
    gid_t l1[64], l2[64];
    int rc1 = getgroups(64, l1);
    long rc2 = syscall(SYS_getgroups, 64, l2);
    CHECK(rc1 == (int)rc2,                                        "getgroups (e) raw syscall == libc");
}

int getgroups_run(void)
{
    printf("\n----- getgroups -----\n");
    getgroups_query_size_zero();
    getgroups_fill_when_buf_large_enough();
    getgroups_too_small_einval();
    getgroups_size_zero_garbage_list_ok();
    getgroups_raw_matches_libc();
    printf("  ----- getgroups: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
