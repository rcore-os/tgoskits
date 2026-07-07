; constmerge.ll - two identical unnamed_addr constants ConstantMerge collapses into one,
; leaving a single matching global. Drives `opt -passes=constmerge`.
@k1 = internal unnamed_addr constant [4 x i8] c"abc\00"
@k2 = internal unnamed_addr constant [4 x i8] c"abc\00"
define ptr @g1() { ret ptr @k1 }
define ptr @g2() { ret ptr @k2 }
