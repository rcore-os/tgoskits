; malformed.ll - deliberately broken IR. llvm-as / llc must REJECT it (non-zero exit),
; proving the parser actually validates rather than silently accepting garbage.
define i32 @broken( {
  %x = frobnicate i32 1, 2
  ret
}
