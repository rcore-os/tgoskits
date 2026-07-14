; unroll.ll - a fixed 4-iteration loop LoopUnroll fully unrolls; after simplifycfg no `phi`
; and no `loop:` block remain. Drives `opt -passes=loop-unroll`.
define i32 @unrolltest() {
entry:
  br label %loop
loop:
  %i = phi i32 [0, %entry], [%i.next, %loop]
  %acc = phi i32 [0, %entry], [%acc.next, %loop]
  %acc.next = add i32 %acc, %i
  %i.next = add i32 %i, 1
  %c = icmp eq i32 %i.next, 4
  br i1 %c, label %done, label %loop
done:
  ret i32 %acc.next
}
