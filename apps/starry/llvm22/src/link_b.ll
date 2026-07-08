; link_b.ll - other half of the llvm-link test: defines @funcB only. After
; `llvm-link link_a.ll link_b.ll` the merged module must carry BOTH functions.
define i32 @funcB() {
  ret i32 22
}
