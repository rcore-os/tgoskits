; instcombine.ll - algebraic identities the -passes=instcombine pass must fold away:
; (x * 1) + 0 == x. After instcombine no `mul`/`add` instruction remains, just `ret %x`.
define i32 @f(i32 %x) {
entry:
  %a = mul i32 %x, 1
  %b = add i32 %a, 0
  ret i32 %b
}
