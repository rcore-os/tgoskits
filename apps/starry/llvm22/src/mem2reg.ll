; mem2reg.ll - two stack slots (alloca/store/load) that the mem2reg / -O2 pipeline
; must promote to SSA registers. Drives `opt`: after promotion NO `alloca` remains.
; compute(x) = (x * 3) + 1
define i32 @compute(i32 %x) {
entry:
  %p = alloca i32
  store i32 %x, ptr %p
  %v = load i32, ptr %p
  %r = mul i32 %v, 3
  %q = alloca i32
  store i32 %r, ptr %q
  %v2 = load i32, ptr %q
  %r2 = add i32 %v2, 1
  ret i32 %r2
}
