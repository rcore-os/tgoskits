; passgen.ll - a module with a global, an alloca, and a counted loop (phi) so the
; opt --print-passes run-clean sweep drives module / cgscc / function / loop passes on
; IR that gives each category something to work on.
@g = global i32 7
define i32 @addfn(i32 %a, i32 %b) {
entry:
  %p = alloca i32
  store i32 %a, ptr %p
  br label %loop
loop:
  %i = phi i32 [ 0, %entry ], [ %i.n, %loop ]
  %i.n = add i32 %i, 1
  %c = icmp slt i32 %i.n, %b
  br i1 %c, label %loop, label %done
done:
  %v = load i32, ptr %p
  %r = add i32 %v, %i.n
  ret i32 %r
}
