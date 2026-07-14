; xcodegen.ll - target-neutral IR (no datalayout / triple) so `llc -march=<t>` retargets
; it to every registered backend. Drives the backend matrix (asm + object per target/opt).
define i32 @addfn(i32 %a, i32 %b) {
entry:
  %s = add i32 %a, %b
  %m = mul i32 %s, 3
  ret i32 %m
}
