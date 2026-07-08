; earlycse.ll - two identical `add` expressions EarlyCSE collapses to one. Drives
; `opt -passes=early-cse` (add count 2 -> 1).
define i32 @ecse(i32 %a, i32 %b) {
  %x = add i32 %a, %b
  %y = add i32 %a, %b
  %z = mul i32 %x, %y
  ret i32 %z
}
