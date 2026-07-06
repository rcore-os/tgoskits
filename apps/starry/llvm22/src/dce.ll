; dce.ll - a dead `mul` (result unused) that DCE/ADCE delete; the tell-tale constant 777
; disappears. Drives `opt -passes=dce` and `-passes=adce`.
define i32 @dcetest(i32 %x) {
entry:
  %dead = mul i32 %x, 777
  %r = add i32 %x, 1
  ret i32 %r
}
