; sccp.ll - a constant condition (1 == 1) that SCCP resolves; followed by simplifycfg the
; live arm `ret i32 100` survives and `ret i32 200` is gone. Drives `opt -passes=sccp`.
define i32 @sccptest() {
entry:
  %c = icmp eq i32 1, 1
  br i1 %c, label %t, label %f
t:
  ret i32 100
f:
  ret i32 200
}
