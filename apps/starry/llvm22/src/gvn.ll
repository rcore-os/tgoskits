; gvn.ll - two identical loads from the same pointer; GVN removes the redundant one,
; leaving a single `load i32`. Drives `opt -passes=gvn`.
define i32 @gvntest(ptr %p) {
entry:
  %a = load i32, ptr %p
  %b = load i32, ptr %p
  %s = add i32 %a, %b
  ret i32 %s
}
