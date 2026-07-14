; simplifycfg.ll - a chain of unconditional branches SimplifyCFG folds into one block,
; leaving zero `br label`. Drives `opt -passes=simplifycfg`.
define i32 @scfgtest(i32 %x) {
entry:
  br label %a
a:
  br label %b
b:
  %r = add i32 %x, 5
  ret i32 %r
}
