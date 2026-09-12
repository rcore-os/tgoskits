/* Scalar-loop implementations of the Intel SVML vector entry points that
 * LLVM 20's SVML VecFuncs table can emit into aarch64 kernels.
 *
 * pocl (misdetected as X86 in this cross-build) bakes the Intel SVML/IRC
 * archive paths into HOST_LD_FLAGS, and its codegen attaches the SVML
 * TargetLibraryInfo unconditionally, so aarch64 kernels that use libm
 * transcendentals get __svml_* vector calls. The real Intel libsvml.a is
 * x86-only machine code with no aarch64 build, so we satisfy the same
 * symbol names with ABI-correct scalar-loop wrappers.
 *
 * Vector params/returns use GCC/Clang vector_size so the AAPCS64 vector
 * passing matches LLVM's <W x T>. The same clang20 --target aarch64 musl
 * -mcpu=cortex-a53 compiles both these wrappers and the kernels, so the
 * ABI is identical by construction.
 *
 * Symbol set = every distinct __svml_* name in
 * llvm20/include/llvm/Analysis/VecFuncs.def lines 313-522 (the
 * TLI_DEFINE_SVML_VECFUNCS block). Duplicate mappings (libm name,
 * llvm.* intrinsic, *_finite) all resolve to the same __svml_* symbol,
 * so each symbol is defined exactly once here.
 */

#include <math.h>

typedef float  f32v4  __attribute__((vector_size(16)));  /* <4  x float>  */
typedef float  f32v8  __attribute__((vector_size(32)));  /* <8  x float>  */
typedef float  f32v16 __attribute__((vector_size(64)));  /* <16 x float>  */
typedef double f64v2  __attribute__((vector_size(16)));  /* <2  x double> */
typedef double f64v4  __attribute__((vector_size(32)));  /* <4  x double> */
typedef double f64v8  __attribute__((vector_size(64)));  /* <8  x double> */

/* 1-arg float: apply scalar libm f-variant per lane, widths 4/8/16 */
#define SVML_F1(name, scalarf)                                           \
  f32v4  __svml_##name##f4 (f32v4  x){ for(int i=0;i<4 ;i++) x[i]=scalarf(x[i]); return x; } \
  f32v8  __svml_##name##f8 (f32v8  x){ for(int i=0;i<8 ;i++) x[i]=scalarf(x[i]); return x; } \
  f32v16 __svml_##name##f16(f32v16 x){ for(int i=0;i<16;i++) x[i]=scalarf(x[i]); return x; }

/* 1-arg double: apply scalar libm d-variant per lane, widths 2/4/8 */
#define SVML_D1(name, scalard)                                           \
  f64v2  __svml_##name##2  (f64v2  x){ for(int i=0;i<2 ;i++) x[i]=scalard(x[i]); return x; } \
  f64v4  __svml_##name##4  (f64v4  x){ for(int i=0;i<4 ;i++) x[i]=scalard(x[i]); return x; } \
  f64v8  __svml_##name##8  (f64v8  x){ for(int i=0;i<8 ;i++) x[i]=scalard(x[i]); return x; }

/* 2-arg float (pow): widths 4/8/16 */
#define SVML_F2(name, scalarf)                                           \
  f32v4  __svml_##name##f4 (f32v4  x, f32v4  y){ for(int i=0;i<4 ;i++) x[i]=scalarf(x[i],y[i]); return x; } \
  f32v8  __svml_##name##f8 (f32v8  x, f32v8  y){ for(int i=0;i<8 ;i++) x[i]=scalarf(x[i],y[i]); return x; } \
  f32v16 __svml_##name##f16(f32v16 x, f32v16 y){ for(int i=0;i<16;i++) x[i]=scalarf(x[i],y[i]); return x; }

/* 2-arg double (pow): widths 2/4/8 */
#define SVML_D2(name, scalard)                                           \
  f64v2  __svml_##name##2  (f64v2  x, f64v2  y){ for(int i=0;i<2 ;i++) x[i]=scalard(x[i],y[i]); return x; } \
  f64v4  __svml_##name##4  (f64v4  x, f64v4  y){ for(int i=0;i<4 ;i++) x[i]=scalard(x[i],y[i]); return x; } \
  f64v8  __svml_##name##8  (f64v8  x, f64v8  y){ for(int i=0;i<8 ;i++) x[i]=scalard(x[i],y[i]); return x; }

/* --- single-arg transcendentals (float f-variant + double variant) --- */
SVML_F1(sin,   sinf)   SVML_D1(sin,   sin)
SVML_F1(cos,   cosf)   SVML_D1(cos,   cos)
SVML_F1(tan,   tanf)   SVML_D1(tan,   tan)
SVML_F1(exp,   expf)   SVML_D1(exp,   exp)
SVML_F1(log,   logf)   SVML_D1(log,   log)
SVML_F1(sqrt,  sqrtf)  SVML_D1(sqrt,  sqrt)

/* log2: double symbols are __svml_log22/24/28, float __svml_log2f4/8/16.
 * The name-collision (exp width-2 == __svml_exp2) is why these need the
 * literal names spelled out rather than the F1/D1 macros. */
f32v4  __svml_log2f4 (f32v4  x){ for(int i=0;i<4 ;i++) x[i]=log2f(x[i]); return x; }
f32v8  __svml_log2f8 (f32v8  x){ for(int i=0;i<8 ;i++) x[i]=log2f(x[i]); return x; }
f32v16 __svml_log2f16(f32v16 x){ for(int i=0;i<16;i++) x[i]=log2f(x[i]); return x; }
f64v2  __svml_log22  (f64v2  x){ for(int i=0;i<2 ;i++) x[i]=log2(x[i]);  return x; }
f64v4  __svml_log24  (f64v4  x){ for(int i=0;i<4 ;i++) x[i]=log2(x[i]);  return x; }
f64v8  __svml_log28  (f64v8  x){ for(int i=0;i<8 ;i++) x[i]=log2(x[i]);  return x; }

/* log10: double __svml_log102/104/108, float __svml_log10f4/8/16 */
f32v4  __svml_log10f4 (f32v4  x){ for(int i=0;i<4 ;i++) x[i]=log10f(x[i]); return x; }
f32v8  __svml_log10f8 (f32v8  x){ for(int i=0;i<8 ;i++) x[i]=log10f(x[i]); return x; }
f32v16 __svml_log10f16(f32v16 x){ for(int i=0;i<16;i++) x[i]=log10f(x[i]); return x; }
f64v2  __svml_log102  (f64v2  x){ for(int i=0;i<2 ;i++) x[i]=log10(x[i]);  return x; }
f64v4  __svml_log104  (f64v4  x){ for(int i=0;i<4 ;i++) x[i]=log10(x[i]);  return x; }
f64v8  __svml_log108  (f64v8  x){ for(int i=0;i<8 ;i++) x[i]=log10(x[i]);  return x; }

/* exp2: double __svml_exp22/24/28, float __svml_exp2f4/8/16.
 * Note __svml_exp2 == exp width-2 (defined by SVML_D1(exp,...) above);
 * exp2 width-2 is __svml_exp22. */
f32v4  __svml_exp2f4 (f32v4  x){ for(int i=0;i<4 ;i++) x[i]=exp2f(x[i]); return x; }
f32v8  __svml_exp2f8 (f32v8  x){ for(int i=0;i<8 ;i++) x[i]=exp2f(x[i]); return x; }
f32v16 __svml_exp2f16(f32v16 x){ for(int i=0;i<16;i++) x[i]=exp2f(x[i]); return x; }
f64v2  __svml_exp22  (f64v2  x){ for(int i=0;i<2 ;i++) x[i]=exp2(x[i]);  return x; }
f64v4  __svml_exp24  (f64v4  x){ for(int i=0;i<4 ;i++) x[i]=exp2(x[i]);  return x; }
f64v8  __svml_exp28  (f64v8  x){ for(int i=0;i<8 ;i++) x[i]=exp2(x[i]);  return x; }

/* --- two-arg (pow) --- */
SVML_F2(pow, powf)  SVML_D2(pow, pow)
