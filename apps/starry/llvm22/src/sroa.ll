; sroa.ll - an aggregate stack slot the SROA pass splits + promotes to registers,
; leaving zero `alloca`. Drives `opt -passes=sroa`.
%pair = type { i32, i32 }
define i32 @sroatest(i32 %a, i32 %b) {
entry:
  %p = alloca %pair
  %f0 = getelementptr %pair, ptr %p, i32 0, i32 0
  store i32 %a, ptr %f0
  %f1 = getelementptr %pair, ptr %p, i32 0, i32 1
  store i32 %b, ptr %f1
  %x = load i32, ptr %f0
  %y = load i32, ptr %f1
  %s = add i32 %x, %y
  ret i32 %s
}
