/*
 * test_x87_fresh_stack.c — a freshly created thread must start with an EMPTY
 * x87 register stack, not a "full" one.
 *
 * 回归背景 (为什么写这个测例):
 *   components/axcpu x86_64 ExtendedState::default() 此前把 FXSAVE 区的 abridged
 *   x87 tag word(ftw)播种成 0xFFFF, 其低字节 0xFF = 八个 x87 寄存器全部标记为
 *   occupied -> "满栈"。在 FXSAVE-fallback 路径(无 XSAVE 的 CPU/VM, 典型 qemu64)
 *   上, 每个新任务 FXRSTOR 回来都带着满栈, 第一条 fld/fild 即栈溢出 -> x87 不定值
 *   (indefinite), 进而打爆 musl 的 x87 long-double fmt_fp(x86 java SIGSEGV storm 根因)。
 *   修复: ftw = 0x0000 (空栈)。
 *
 * 本测例: 在新 pthread 里**不做 fninit/fclex**(否则会把 tag word 清空、掩盖本 bug),
 *   直接在 FXRSTOR 加载的初始状态上连续 8 次 fld(压栈), 读 x87 status word(fnstsw)。
 *   空栈起步: 8 次压栈恰好填满 ST0..ST7, 不溢出 -> 无栈错误(SF/IE 不置位)。
 *   满栈起步: 第一次 fld 就溢出 -> status word 的 IE(invalid)+SF(stack fault) 置位,
 *   且 ST0 读回为 x87 indefinite。修复前 FAIL, 修复后 PASS。
 *
 * 注意(诚实说明): 该 bug 仅在 FXSAVE-fallback(qemu64)复现; system group 的
 *   qemu-x86_64.toml 用 -cpu Haswell,+avx 走 XSAVE/XRSTOR, 那里 x87 由零化的
 *   XSTATE_BV header 重新初始化, 故本测例在该模型上修复前后都通过。它是一条
 *   "新线程 x87 空栈" 不变量守卫; 端到端复现以 JVM carpet(JAVA_GRAMMAR/JAVA_LANG,
 *   走 musl long-double fmt_fp)为准。
 */

#include "test_framework.h"

#if defined(__x86_64__)

#include <pthread.h>
#include <stdint.h>

/* x87 status word bits */
#define X87_SW_IE  0x0001u   /* invalid operation */
#define X87_SW_SF  0x0040u   /* stack fault */

static int g_stack_fault;     /* set by worker if a push overflowed */
static long double g_st0;     /* value read back from ST(0) after 8 pushes */

static void *worker(void *arg)
{
    (void)arg;
    uint16_t sw = 0;
    long double one = 1.0L;
    long double st0 = 0.0L;

    __asm__ volatile(
        /* NO fninit/fclex here on purpose: those would reset the x87 tag word to
         * empty and mask exactly the bug under test. We must observe the state the
         * kernel's FXRSTOR (ExtendedState::default) loaded at thread birth.
         * 8 pushes: on an EMPTY stack (correct: ftw abridged 0x00) they exactly
         * fill ST0..ST7 with no overflow; on a "full" stack (buggy: ftw 0xFF) the
         * very first fld overflows -> IE|SF set in the status word. */
        "fldt %[one]            \n\t"
        "fldt %[one]            \n\t"
        "fldt %[one]            \n\t"
        "fldt %[one]            \n\t"
        "fldt %[one]            \n\t"
        "fldt %[one]            \n\t"
        "fldt %[one]            \n\t"
        "fldt %[one]            \n\t"
        "fnstsw %[sw]           \n\t"   /* capture status word */
        "fstpt %[st0]           \n\t"   /* pop ST0 back to memory */
        "fninit                 \n\t"   /* leave FPU clean for the thread */
        : [sw] "=m"(sw), [st0] "=m"(st0)
        : [one] "m"(one)
        : "memory");

    g_stack_fault = (sw & (X87_SW_IE | X87_SW_SF)) ? 1 : 0;
    g_st0 = st0;
    return NULL;
}

int main(void)
{
    TEST_START("fresh pthread starts with EMPTY x87 stack (ftw=0)");

    pthread_t t;
    int rc = pthread_create(&t, NULL, worker, NULL);
    CHECK_RET(rc, 0, "pthread_create worker");
    if (rc == 0) {
        pthread_join(t, NULL);
        /* 空栈起步: 8 次压栈不溢出 -> 无 stack fault/invalid; ST0 读回为 1.0 */
        CHECK(g_stack_fault == 0,
              "8 fld on a fresh thread did NOT overflow (empty x87 stack)");
        CHECK(g_st0 == 1.0L,
              "ST(0) holds the pushed value 1.0 (not x87 indefinite)");
    }
    TEST_DONE();
}

#else /* !__x86_64__ */

int main(void)
{
    TEST_START("fresh pthread starts with EMPTY x87 stack (ftw=0)");
    CHECK(1, "non-x86_64: x87 fresh-stack test skipped");
    TEST_DONE();
}

#endif
