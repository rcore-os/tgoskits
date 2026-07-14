; tailcall.ll - a self-recursive tail call TailCallElim turns into a loop; the recursive
; `call i32 @tcetest` disappears. Drives `opt -passes=tailcallelim`.
define i32 @tcetest(i32 %n, i32 %acc) {
entry:
  %c = icmp eq i32 %n, 0
  br i1 %c, label %base, label %rec
base:
  ret i32 %acc
rec:
  %n1 = sub i32 %n, 1
  %acc1 = add i32 %acc, %n
  %r = call i32 @tcetest(i32 %n1, i32 %acc1)
  ret i32 %r
}
