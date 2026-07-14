; licm.ll - a loop-invariant `mul` LICM hoists out of the loop (still exactly one `mul`).
; Drives `opt -passes='loop-mssa(licm)'` (verifier-valid output).
define i32 @licmtest(i32 %n, i32 %a, i32 %b) {
entry:
  br label %loop
loop:
  %i = phi i32 [0, %entry], [%i.n, %loop]
  %acc = phi i32 [0, %entry], [%acc.n, %loop]
  %inv = mul i32 %a, %b
  %acc.n = add i32 %acc, %inv
  %i.n = add i32 %i, 1
  %c = icmp slt i32 %i.n, %n
  br i1 %c, label %loop, label %done
done:
  ret i32 %acc
}
