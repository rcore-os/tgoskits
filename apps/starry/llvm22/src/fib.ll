; fib.ll - recursive Fibonacci, fib(10) = 55, printed via printf. Drives lli JIT
; recursive calls / stack frames + return values.
@.fmt = private unnamed_addr constant [8 x i8] c"FIB=%d\0A\00"

declare i32 @printf(ptr, ...)

define i32 @fib(i32 %n) {
entry:
  %c = icmp slt i32 %n, 2
  br i1 %c, label %base, label %rec
base:
  ret i32 %n
rec:
  %n1 = sub i32 %n, 1
  %n2 = sub i32 %n, 2
  %f1 = call i32 @fib(i32 %n1)
  %f2 = call i32 @fib(i32 %n2)
  %s = add i32 %f1, %f2
  ret i32 %s
}

define i32 @main() {
entry:
  %r = call i32 @fib(i32 10)
  %p = call i32 (ptr, ...) @printf(ptr @.fmt, i32 %r)
  ret i32 0
}
