; hello.ll - LLVM 22 textual IR: puts a fixed message, main returns 0.
; Drives: lli JIT (exact stdout), llc (asm/obj codegen), llvm-as/llvm-dis roundtrip, FileCheck.
@.msg = private unnamed_addr constant [16 x i8] c"Hello, LLVM 22!\00"

declare i32 @puts(ptr)

define i32 @main() {
entry:
  %r = call i32 @puts(ptr @.msg)
  ret i32 0
}
