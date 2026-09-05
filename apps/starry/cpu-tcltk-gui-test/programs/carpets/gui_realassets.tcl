# gui_realassets.tcl - real font metrics leg: use a real (system/staged) font family and assert closed-form
# `font measure` / `font metrics` values, plus canvas text bbox that scales with pixel size.
#
# Unlike Qt (QFontDatabase::addApplicationFont loads a .ttf directly), base Tk 8.6 has NO application-font
# file-load API: Tk consumes fonts the X server / fontconfig already knows. So this leg's "real asset" is a
# real, resolvable font *family* - it prefers a monospace family whose deterministic metrics we can predict
# from first principles (a fixed-pitch font's N-char measure == N * single-char width, exactly), and it
# HONEST-SKIPS (still prints its OK marker with the skip check) when no suitable real family is resolvable,
# so the synthetic legs (render/layout/interact) always gate on their own.
#
# When a real monospace family IS available:
#   - font measure: N glyphs of a fixed-pitch family measure exactly N * one-glyph width.
#   - font metrics: -fixed is 1 (monospace), -linespace == -ascent + -descent (+ optional leading >= sum),
#     and every value is a positive integer.
#   - canvas text: rendering the same string at a larger pixel size yields a strictly wider bbox (the font
#     engine scales), while at a fixed size the bbox is bounded and non-degenerate.
#
# ASSET_DIR (default /opt/cpu-tcltk-gui-test/assets) may hold a staged .ttf; if its family name (derived from
# the file name, e.g. DejaVuSansMono -> "DejaVu Sans Mono") is resolvable we assert on it, else we fall back
# to any resolvable monospace family, else honest-skip. Font-agnostic: metrics relations, never glyph pixels.

package require Tk
wm withdraw .
source [file join [file dirname [info script]] gui_common.tcl]
gate_init GUI_REALASSETS

proc asset_dir {} {
    if {[info exists ::env(ASSET_DIR)] && $::env(ASSET_DIR) ne ""} { return $::env(ASSET_DIR) }
    if {[info exists ::env(FONT_DIR)]  && $::env(FONT_DIR)  ne ""} { return $::env(FONT_DIR) }
    return "/opt/cpu-tcltk-gui-test/assets"
}

# Turn a staged font file basename into a candidate Tk family, e.g. DejaVuSansMono.ttf -> "DejaVu Sans Mono".
proc file_to_family {path} {
    set base [file rootname [file tail $path]]
    regsub -all {[-_]} $base " " base
    # split camelCase boundaries: insert a space between a lower/digit and an upper
    regsub -all {([a-z0-9])([A-Z])} $base {\1 \2} base
    return [string trim $base]
}

# Return the first resolvable font family for this asset dir: a staged-asset-derived family if present and
# recognized, else any resolvable monospace family, else "".
proc pick_real_family {} {
    set dir [asset_dir]
    set fams [font families]
    # 1) prefer a staged asset whose derived family Tk recognizes
    if {[file isdirectory $dir]} {
        foreach f [lsort [glob -nocomplain -directory $dir *.ttf *.otf *.ttc]] {
            set cand [file_to_family $f]
            if {[lsearch -exact $fams $cand] >= 0} { return $cand }
        }
    }
    # 2) any resolvable well-known monospace family
    foreach cand {"DejaVu Sans Mono" "Liberation Mono" "Noto Sans Mono" "Courier New"} {
        if {[lsearch -exact $fams $cand] >= 0} { return $cand }
    }
    # 3) the Tk logical "Courier" alias resolves to a fixed font on any Tk build
    set probe [font create -family Courier -size -20]
    set fixed [font metrics $probe -fixed]
    font delete $probe
    if {$fixed == 1} { return "Courier" }
    return ""
}

set family [pick_real_family]

if {$family eq ""} {
    puts stderr "  gui_realassets: no resolvable real font family (asset dir [asset_dir]) - honest skip"
    check {1 == 1} "realassets honest-skip (no resolvable font family)"
    gate_finish
}

puts stderr "  gui_realassets: using real font family '$family'"

# ------------------------------------------------------------------ font measure: N-char == N * one-char
set f [font create -family $family -size -20]
set w1 [font measure $f "X"]
set w5 [font measure $f "XXXXX"]
check {$w1 > 0}            "realassets: single-glyph measure not positive"
check {$w5 == 5 * $w1}     "realassets: 5-char measure ($w5) != 5 * one-char ($w1) for a fixed-pitch font"
set w10 [font measure $f "XXXXXXXXXX"]
check {$w10 == 10 * $w1}   "realassets: 10-char measure != 10 * one-char"

# ------------------------------------------------------------------ font metrics: closed-form relations
set ls [font metrics $f -linespace]
set asc [font metrics $f -ascent]
set desc [font metrics $f -descent]
set fixed [font metrics $f -fixed]
check {$fixed == 1}                    "realassets: family not reported fixed-pitch"
check {$asc > 0 && $desc >= 0}         "realassets: ascent/descent not positive"
check {$ls >= $asc + $desc}            "realassets: linespace < ascent + descent"

# ------------------------------------------------------------------ canvas text bbox scales with pixel size
set c [canvas .rac -width 300 -height 120 -highlightthickness 0]
pack $c
set small [font create -family $family -size -12]
set big   [font create -family $family -size -28]
set ts [$c create text 10 30 -text "Starry" -anchor sw -font $small]
set tb [$c create text 10 90 -text "Starry" -anchor sw -font $big]
update idletasks
set bbs [$c bbox $ts]; set bbb [$c bbox $tb]
check {$bbs ne {} && $bbb ne {}} "realassets: canvas text bbox empty"
if {$bbs ne {} && $bbb ne {}} {
    lassign $bbs sx0 sy0 sx1 sy1
    lassign $bbb bx0 by0 bx1 by1
    check {$sx1 > $sx0 && $sy1 > $sy0}   "realassets: small text bbox degenerate"
    check {($bx1 - $bx0) > ($sx1 - $sx0)} "realassets: larger pixel-size did not widen the bbox"
    check {($by1 - $by0) > ($sy1 - $sy0)} "realassets: larger pixel-size did not heighten the bbox"
}
destroy $c
gate_finish
