/*
 * test_getrlimit_legacy.c — legacy getrlimit(2)/setrlimit(2) route to prlimit64.
 *
 * 回归背景: StarryOS 此前只 dispatch prlimit64, 旧的 getrlimit/setrlimit syscall
 * 落到 ENOSYS catch-all. Go 的 syscall 包直接调旧 getrlimit, 导致 consul 等在
 * riscv64 上启动即 SIGABRT. 修复: 两条 match 臂把 legacy syscall 路由到
 * sys_prlimit64(pid=0):
 *   getrlimit(res,*rlim) -> prlimit64(0,res,NULL,rlim)   (old 出参)
 *   setrlimit(res,*rlim) -> prlimit64(0,res,rlim,NULL)   (new 入参)
 *
 * man 2 getrlimit / prlimit64: 旧接口即 prlimit64 的特例; `struct rlimit`(两个
 * unsigned long)与 rlimit64(两个 u64)在 64 位 arch 上布局一致. pid 0 = 调用进程.
 *
 * 用**原始 syscall**(musl 的 getrlimit() 包装内部走 prlimit64, 直接 raw 才测到
 * 旧 syscall 号). 修复前 raw getrlimit -> -ENOSYS(ret=-1); 修复后 -> 0.
 */

#define _GNU_SOURCE
#include "test_framework.h"
#include <sys/resource.h>
#include <sys/syscall.h>
#include <unistd.h>

/* syscall 号在 musl <sys/syscall.h> 对四 arch 均有定义(x86_64 97/160;
 * aarch64/riscv64/loongarch64 163/164). 兜底 define 防个别 sysroot 缺失. */
#ifndef SYS_getrlimit
#define SYS_getrlimit 163
#endif
#ifndef SYS_setrlimit
#define SYS_setrlimit 164
#endif

static long raw_getrlimit(int res, struct rlimit *rl)
{
    return syscall(SYS_getrlimit, res, rl);
}
static long raw_setrlimit(int res, const struct rlimit *rl)
{
    return syscall(SYS_setrlimit, res, rl);
}
static long raw_prlimit64(int res, const struct rlimit *new_, struct rlimit *old)
{
    return syscall(SYS_prlimit64, 0, res, new_, old);
}

int main(void)
{
    TEST_START("legacy getrlimit/setrlimit route to prlimit64");

    /* 1. raw getrlimit(RLIMIT_NOFILE) 成功(修复前 -ENOSYS) */
    struct rlimit r = {0, 0};
    CHECK_RET(raw_getrlimit(RLIMIT_NOFILE, &r), 0, "raw getrlimit(RLIMIT_NOFILE) 返回 0");
    CHECK(r.rlim_cur <= r.rlim_max, "getrlimit: cur <= max");

    /* 2. 与 prlimit64 交叉验证一致 */
    struct rlimit rp = {0, 0};
    CHECK_RET(raw_prlimit64(RLIMIT_NOFILE, NULL, &rp), 0, "prlimit64 取值(交叉参照)");
    CHECK(r.rlim_cur == rp.rlim_cur && r.rlim_max == rp.rlim_max,
          "legacy getrlimit 与 prlimit64 返回一致");

    /* 3. raw setrlimit 生效(把 cur 设为 max, 合法且可逆) */
    struct rlimit nr = r;
    nr.rlim_cur = r.rlim_max;
    CHECK_RET(raw_setrlimit(RLIMIT_NOFILE, &nr), 0, "raw setrlimit(RLIMIT_NOFILE, cur=max) 返回 0");
    struct rlimit after = {0, 0};
    raw_prlimit64(RLIMIT_NOFILE, NULL, &after);
    CHECK(after.rlim_cur == nr.rlim_cur, "setrlimit 生效(经 prlimit64 可见)");

    /* 4. 另一资源 getrlimit(RLIMIT_STACK) 也成功(覆盖 res 参数透传) */
    struct rlimit rs = {0, 0};
    CHECK_RET(raw_getrlimit(RLIMIT_STACK, &rs), 0, "raw getrlimit(RLIMIT_STACK) 返回 0");

    /* 5. setrlimit 还原 cur(不污染后续 fd 限制) */
    CHECK_RET(raw_setrlimit(RLIMIT_NOFILE, &r), 0, "raw setrlimit 还原原值");

    TEST_DONE();
}
