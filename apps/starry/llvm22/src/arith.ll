; arith.ll - main returns a computed exit status. Drives lli JIT exit-code path.
; (40 + 2) * 1 = 42  ->  process exit code 42.
define i32 @main() {
entry:
  %a = add i32 40, 2
  %b = mul i32 %a, 1
  ret i32 %b
}
