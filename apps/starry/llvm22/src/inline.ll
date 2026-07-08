; inline.ll - one internal callee called exactly once. The -passes=inline inliner
; must splice @callee into @caller, leaving zero `call i32 @callee` sites.
define internal i32 @callee(i32 %x) {
  %r = add i32 %x, 7
  ret i32 %r
}

define i32 @caller() {
  %r = call i32 @callee(i32 35)
  ret i32 %r
}
