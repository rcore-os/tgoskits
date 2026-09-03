/*
 * !test-archprctl-tls — arch_prctl(2) x86-64 TLS + 各 arch TLS 寄存器 + pkey 降级 测试
 *
 * ground truth: man 2 arch_prctl / pkey_alloc + Linux v7.2
 * arch/x86/kernel/process_64.c(do_arch_prctl_64)。覆盖 V8/JIT TLS 命脉:
 *   x86-64: arch_prctl(ARCH_SET_FS/GET_FS/SET_GS/GET_GS) 设/读 fs/gs base;
 *   全 arch: 线程 TLS 寄存器(x86 fs / aarch64 tpidr_el0 / riscv tp / loong $tp)
 *     由 libc 建好非 0; pkey_alloc 无 MPK 时优雅降级(浏览器可选)。
 *
 * =====================================================================
 * 语义 (man 2 arch_prctl)
 * =====================================================================
 *   ARCH_SET_FS(0x1002)/GET_FS(0x1003): 设/读 %fs base(x86-64 TLS 基址);
 *   ARCH_SET_GS(0x1001)/GET_GS(0x1004): 设/读 %gs base; 非法 code -> EINVAL。
 *   SET_FS/SET_GS 在 Linux 恒不返回错误。x86-64 专有(其他 arch 无此 syscall)。
 *   pkey_alloc(2): 分配保护键(MPK); 无硬件/未实现 -> ENOSYS/EINVAL(可降级)。
 *
 * =====================================================================
 * Linux/StarryOS 对齐
 * =====================================================================
 *   StarryOS sys_arch_prctl(thread.rs): GetFs 写 uctx.tls(); SetFs set_tls;
 *   GetGs/SetGs 读写 gs_base; 非法 code -> InvalidInput(EINVAL)。x86-64 only。
 * =====================================================================
 */

#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
#include "test_framework.h"

#include <stddef.h>
#include <stdint.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <signal.h>
#include <unistd.h>
#include <string.h>

#ifndef ARCH_SET_GS
#define ARCH_SET_GS 0x1001
#define ARCH_SET_FS 0x1002
#define ARCH_GET_FS 0x1003
#define ARCH_GET_GS 0x1004
#endif

#ifndef ARCH_GET_CPUID
#define ARCH_GET_CPUID 0x1011
#define ARCH_SET_CPUID 0x1012
#endif
#ifndef ARCH_CPUID_ENABLE
#define ARCH_CPUID_ENABLE 1
#define ARCH_CPUID_SIGSEGV 0
#endif

static void alarm_handler(int s)
{
    (void)s;
    const char *m = "\n  FAIL | TIMEOUT | 测试挂死\n==== test-archprctl-tls 汇总: FAIL ====\n";
    ssize_t r = write(2, m, strlen(m));
    (void)r;
    _exit(1);
}

/* 读本线程 TLS 寄存器(不经 arch_prctl, 各 arch inline asm)。 */
static unsigned long read_tls_reg(void)
{
#if defined(__x86_64__)
    unsigned long v = 0;
    syscall(SYS_arch_prctl, ARCH_GET_FS, &v);
    return v;
#elif defined(__aarch64__)
    unsigned long v;
    __asm__ volatile("mrs %0, tpidr_el0" : "=r"(v));
    return v;
#elif defined(__riscv)
    unsigned long v;
    __asm__ volatile("mv %0, tp" : "=r"(v));
    return v;
#elif defined(__loongarch__)
    unsigned long v;
    __asm__ volatile("move %0, $tp" : "=r"(v));
    return v;
#else
    return 0;
#endif
}

/* ===== A. 各 arch: 线程 TLS 寄存器已由 libc 建好(非 0) ===== */
static int test_tls_reg_setup(void)
{
    TEST_START("A. 线程 TLS 寄存器由 libc 建好(非 0)");
    unsigned long tls = read_tls_reg();
    CHECK(tls != 0, "TLS 寄存器非 0(libc 初始化了线程 TLS)");
    TEST_DONE();
}

#if defined(__x86_64__)
/* ★关键: SET_FS 到测试值后, 任何 fs 相对访问(errno/printf/栈金丝雀)都会读错 TLS。
 * 故 SET_FS(测试值)->GET_FS(读回栈局部)->SET_FS(原值恢复) 必须紧凑, 中间不触
 * 任何 libc TLS 访问; 恢复后再 CHECK/printf。GET_FS 只写栈局部 got, 安全。 */
static int test_arch_prctl_fs(void)
{
    TEST_START("B. arch_prctl ARCH_SET_FS/GET_FS 往返(x86)");
    unsigned long orig = 0, got = 0;
    /* mmap 一页作合法 fs 测试值(即使中途有 fs 访问也不崩) */
    void *page = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    unsigned long testv = (page == MAP_FAILED) ? 0x0 : (unsigned long)page + 0x40;

    syscall(SYS_arch_prctl, ARCH_GET_FS, &orig); /* 存原 musl TLS */
    syscall(SYS_arch_prctl, ARCH_SET_FS, testv);
    long repeat_r = syscall(SYS_arch_prctl, ARCH_SET_FS, testv);
    syscall(SYS_arch_prctl, ARCH_GET_FS, &got);
    syscall(SYS_arch_prctl, ARCH_SET_FS, orig); /* 恢复! 之后才可 CHECK/printf */

    CHECK(repeat_r == 0, "重复 ARCH_SET_FS 同值成功(懒安装零额外写路径)");
    CHECK(got == testv, "ARCH_SET_FS 后 GET_FS 读回同值");
    CHECK(read_tls_reg() == orig, "ARCH_SET_FS 恢复原 TLS 成功");
    if (page != MAP_FAILED) munmap(page, 4096);
    TEST_DONE();
}

static int test_arch_prctl_gs_errno(void)
{
    TEST_START("C. arch_prctl ARCH_SET_GS/GET_GS + 非法 code EINVAL(x86)");
    /* gs base: musl 不用 gs 作 TLS, 可自由设 */
    unsigned long gv = 0xdead000;
    long r = syscall(SYS_arch_prctl, ARCH_SET_GS, gv);
    CHECK(r == 0, "ARCH_SET_GS 成功(恒不返错)");
    r = syscall(SYS_arch_prctl, ARCH_SET_GS, gv);
    CHECK(r == 0, "重复 ARCH_SET_GS 同值成功(懒安装零额外写路径)");
    unsigned long got = 0;
    r = syscall(SYS_arch_prctl, ARCH_GET_GS, &got);
    CHECK(r == 0 && got == gv, "ARCH_GET_GS 读回同值");
    syscall(SYS_arch_prctl, ARCH_SET_GS, 0); /* 复位 gs */

    /* 非法 code -> EINVAL */
    errno = 0;
    r = syscall(SYS_arch_prctl, 0x9999, 0);
    CHECK(r == -1 && errno == EINVAL, "非法 arch_prctl code -> EINVAL");
    TEST_DONE();
}
#endif /* __x86_64__ */

/* ===== D. pkey_alloc 优雅降级(无 MPK / 未实现) ===== */
static int test_pkey_graceful(void)
{
    TEST_START("D. pkey_alloc 优雅降级(浏览器可选 MPK)");
#ifdef SYS_pkey_alloc
    errno = 0;
    long key = syscall(SYS_pkey_alloc, 0, 0);
    /* MPK 存在 -> 返回 key>=0; 无 MPK/未实现 -> -1 + ENOSYS/EINVAL/EOPNOTSUPP。
     * 二者皆合法: 浏览器在无 MPK 时降级不用保护键。关键是不崩、返回值理智。 */
    int ok = (key >= 0) ||
             (key == -1 && (errno == ENOSYS || errno == EINVAL || errno == EOPNOTSUPP));
    CHECK(ok, "pkey_alloc 返回 key 或优雅错误(不崩)");
    if (key >= 0) {
        syscall(SYS_pkey_free, key);
    }
#else
    CHECK(1, "本 libc 无 SYS_pkey_alloc 定义(跳过, 等价未实现降级)");
#endif
    TEST_DONE();
}

#if defined(__x86_64__)
/*
 * arch_prctl(ARCH_GET_CPUID/ARCH_SET_CPUID) — x86-64 CPUID faulting control.
 * ground truth: man 2 arch_prctl, Linux arch/x86/kernel/process.c
 *   get_cpuid_mode()/set_cpuid_mode().
 *   ARCH_GET_CPUID returns ARCH_CPUID_ENABLE(1) when the CPUID instruction is
 *   enabled for the thread and ARCH_CPUID_SIGSEGV(0) when it faults. A thread
 *   starts with CPUID enabled, so a freshly-started process reads back 1.
 *   ARCH_SET_CPUID returns -ENODEV on a system without CPUID-faulting support
 *   (StarryOS never installs faulting), for every requested value.
 */
static int test_arch_prctl_cpuid(void)
{
    TEST_START("D. arch_prctl ARCH_GET_CPUID/ARCH_SET_CPUID(x86)");
    errno = 0;
    long r = syscall(SYS_arch_prctl, ARCH_GET_CPUID, 0);
    CHECK(r == ARCH_CPUID_ENABLE,
          "ARCH_GET_CPUID 报 CPUID 已启用(1),线程默认可执行 CPUID");
    /* No CPUID-faulting support: SET_CPUID is ENODEV for any value. */
    CHECK_ERR(syscall(SYS_arch_prctl, ARCH_SET_CPUID, ARCH_CPUID_SIGSEGV), ENODEV,
              "ARCH_SET_CPUID(禁用) 无 faulting 支持 -> ENODEV");
    CHECK_ERR(syscall(SYS_arch_prctl, ARCH_SET_CPUID, ARCH_CPUID_ENABLE), ENODEV,
              "ARCH_SET_CPUID(启用) 无 faulting 支持 -> ENODEV");
    TEST_DONE();
}
#endif

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    signal(SIGALRM, alarm_handler);
    alarm(60);
    int fail = 0;
    fail |= test_tls_reg_setup();
#if defined(__x86_64__)
    fail |= test_arch_prctl_fs();
    fail |= test_arch_prctl_gs_errno();
    fail |= test_arch_prctl_cpuid();
#endif
    fail |= test_pkey_graceful();
    printf("\n==== test-archprctl-tls 汇总: %s ====\n", fail ? "FAIL" : "PASS");
    return fail;
}
