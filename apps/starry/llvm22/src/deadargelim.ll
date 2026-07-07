; deadargelim.ll - an unused callee argument DeadArgElim drops; the passed constant 123
; disappears from the call. Drives `opt -passes=deadargelim`.
define internal i32 @callee(i32 %a, i32 %unused) {
  ret i32 %a
}
define i32 @caller(i32 %x) {
  %r = call i32 @callee(i32 %x, i32 123)
  ret i32 %r
}
