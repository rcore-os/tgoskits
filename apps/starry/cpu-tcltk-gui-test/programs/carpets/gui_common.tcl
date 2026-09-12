# gui_common.tcl - shared primitives for the cpu-tcltk-gui-test carpet (a "pyte for GUI widgets").
#
# Each cell drives a real Tcl/Tk pipeline against an X server provided headlessly by Xvfb (virtual
# framebuffer, no physical display) and asserts the result against a CLOSED-FORM golden: exact per-pixel
# colors from known photo-image put/copy geometry, exact canvas item geometry from Tk's own canvas layout
# (coords/bbox), exact pack/grid/place child geometry from the geometry-manager arithmetic, exact font
# measure/metrics, and post-event widget state from injected `event generate` mouse/key events.
# "Widget created" alone is NOT a test here - every leg checks a value it can predict from first principles.
#
# Why photo images for pixels: base Tk 8.6 hands back exact pixels only from a `photo` image (Tk's real
# image surface: `$img get x y` reads the stored RGB after `put`/`copy` compositing). Grabbing a live canvas
# to pixels needs the Img/tkimg `window` photo format or a Ghostscript rasterizer for `canvas postscript`,
# neither of which base Tk ships; so the render leg asserts photo pixels (Tk_PhotoImage engine) for exact
# color/compositing and asserts canvas *geometry* (coords/bbox/find - Tk's canvas layout engine) separately.
#
# Determinism: fixed sizes, fixed named colors, fixed-pixel fonts, and a fixed seed (0x233) wherever a random
# path could appear. Text legs assert a bbox + non-empty extent (glyph pixels depend on the bundled font);
# never glyph-exact pixels.
#
# Three-gate marker: a cell prints "GUI_<CELL> OK <n>" only when fail==0 && total==pass && total>0.

# fixed seed anywhere randomness could enter (none of the legs are random, but pin it for reproducibility)
expr {srand(0x233)}

# ------------------------------------------------------------------ three-gate marker
namespace eval gate {
    variable pass 0
    variable total 0
    variable fail 0
    variable name "GUI"
}

proc gate_init {name} {
    set gate::pass 0
    set gate::total 0
    set gate::fail 0
    set gate::name $name
}

# check {cond} {msg} - increment total; pass if the boolean expression `cond` evaluates true, else fail and
# print the message to stderr. `cond` is evaluated with [expr] in the caller's context so both bare boolean
# results (from [rect_is_color ...] which already returns 0/1) and comparison expressions (== / eq / &&) work.
proc check {cond msg} {
    incr gate::total
    set ok [uplevel 1 [list expr $cond]]
    if {$ok} {
        incr gate::pass
    } else {
        incr gate::fail
        puts stderr "  FAIL: $msg"
    }
}

# finish - print the three-gate marker and exit with 0 (all pass) or 1 (any fail / empty).
proc gate_finish {} {
    if {$gate::fail == 0 && $gate::total == $gate::pass && $gate::total > 0} {
        puts "$gate::name OK $gate::total"
        exit 0
    }
    puts "$gate::name FAILED pass=$gate::pass total=$gate::total fail=$gate::fail"
    exit 1
}

# ------------------------------------------------------------------ photo-image pixel helpers
# `$img get x y` returns a list {R G B} of 0..255. These helpers turn that into exact comparisons.

# rgb_eq {pixel} {r g b} {tol} - channel-wise |a-b| <= tol on every channel.
proc rgb_eq {pixel r g b tol} {
    lassign $pixel pr pg pb
    expr {abs($pr - $r) <= $tol && abs($pg - $g) <= $tol && abs($pb - $b) <= $tol}
}

# pix_is {img x y} {r g b} {tol} - the pixel at (x,y) is within tol of (r,g,b).
proc pix_is {img x y r g b tol} {
    rgb_eq [$img get $x $y] $r $g $b $tol
}

# rect_is_color {img x0 y0 x1 y1} {r g b} {tol} - every pixel in [x0,x1) x [y0,y1) is within tol of the color.
proc rect_is_color {img x0 y0 x1 y1 r g b tol} {
    for {set y $y0} {$y < $y1} {incr y} {
        for {set x $x0} {$x < $x1} {incr x} {
            if {![rgb_eq [$img get $x $y] $r $g $b $tol]} { return 0 }
        }
    }
    return 1
}

# count_color {img x0 y0 x1 y1} {r g b} {tol} - number of pixels in the rect within tol of the color.
proc count_color {img x0 y0 x1 y1 r g b tol} {
    set n 0
    for {set y $y0} {$y < $y1} {incr y} {
        for {set x $x0} {$x < $x1} {incr x} {
            if {[rgb_eq [$img get $x $y] $r $g $b $tol]} { incr n }
        }
    }
    return $n
}

# ------------------------------------------------------------------ ink helpers (text / glyph legs)
# "ink" = a pixel that differs from the background by more than tol on any channel.

proc count_non_bg {img w h br bg bb tol} {
    set n 0
    for {set y 0} {$y < $h} {incr y} {
        for {set x 0} {$x < $w} {incr x} {
            if {![rgb_eq [$img get $x $y] $br $bg $bb $tol]} { incr n }
        }
    }
    return $n
}

# ink_bbox - tight bounding box of ink pixels. Returns {} if no ink, else {minx miny maxx maxy}.
proc ink_bbox {img w h br bg bb tol} {
    set lx $w; set ly $h; set hx -1; set hy -1
    for {set y 0} {$y < $h} {incr y} {
        for {set x 0} {$x < $w} {incr x} {
            if {![rgb_eq [$img get $x $y] $br $bg $bb $tol]} {
                if {$x < $lx} { set lx $x }
                if {$x > $hx} { set hx $x }
                if {$y < $ly} { set ly $y }
                if {$y > $hy} { set hy $y }
            }
        }
    }
    if {$hx < 0} { return {} }
    return [list $lx $ly $hx $hy]
}
