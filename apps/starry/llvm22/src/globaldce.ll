; globaldce.ll - an unreferenced internal global GlobalDCE removes; @unused disappears.
; Drives `opt -passes=globaldce`.
@used = internal constant i32 5
@unused = internal constant i32 99
define i32 @gdcetest() {
  %v = load i32, ptr @used
  ret i32 %v
}
