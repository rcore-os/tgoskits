; loop.ll - counted loop summing 1..100 = 5050, printed via printf. Drives lli JIT
; control flow (phi nodes / branches) + libc call resolution.
@.fmt = private unnamed_addr constant [8 x i8] c"SUM=%d\0A\00"

declare i32 @printf(ptr, ...)

define i32 @main() {
entry:
  br label %loop
loop:
  %i = phi i32 [ 1, %entry ], [ %ni, %body ]
  %s = phi i32 [ 0, %entry ], [ %ns, %body ]
  %c = icmp sle i32 %i, 100
  br i1 %c, label %body, label %done
body:
  %ns = add i32 %s, %i
  %ni = add i32 %i, 1
  br label %loop
done:
  %r = call i32 (ptr, ...) @printf(ptr @.fmt, i32 %s)
  ret i32 0
}
