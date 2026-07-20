; reassoc.ll - an add tree Reassociate rewrites into a canonical form. Drives
; `opt -passes=reassociate` (verifier-valid output).
define i32 @reasstest(i32 %a, i32 %b, i32 %c) {
  %t1 = add i32 %a, %b
  %t2 = add i32 %t1, %c
  ret i32 %t2
}
