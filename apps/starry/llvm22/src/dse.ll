; dse.ll - a store immediately overwritten; DSE deletes the first (`store i32 1`) and keeps
; the second (`store i32 2`). Drives `opt -passes=dse`.
define void @dsetest(ptr %p) {
  store i32 1, ptr %p
  store i32 2, ptr %p
  ret void
}
