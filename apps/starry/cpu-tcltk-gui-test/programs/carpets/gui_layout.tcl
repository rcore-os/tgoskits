# gui_layout.tcl - deterministic geometry: assert each child's realized geometry == closed-form layout math.
#
# Widgets are given fixed sizes and the geometry managers fixed padding, the toplevel is realized and the
# event loop flushed (update idletasks) so Tk actually performs the layout, and each child's position/size
# is read back via `winfo x/y/width/height/reqwidth/reqheight` and compared to exact arithmetic:
#   - place: a child placed at -x/-y sits at exactly those window coords (winfo x/y).
#   - pack (side top): children stack vertically; each child's y is the running sum of prior heights + pady.
#   - grid: `grid info` reports the exact row/column/span; grid bbox gives the exact cell rectangle.
#   - a labelframe with -padx/-pady contains its child and its reqwidth = child reqwidth + 2*pad + border.
#   - reqwidth/reqheight of a fixed-size widget equal the requested size (closed form).
#
# No pixels needed: geometry-manager math is exact integer arithmetic, identical across arch. Xvfb provides
# the headless X display so the managers actually map and size the widgets.

package require Tk
source [file join [file dirname [info script]] gui_common.tcl]
gate_init GUI_LAYOUT

# a fixed-size label whose reqwidth/reqheight are pinned by -width/-height in *pixels* via a frame wrapper.
proc fixed_frame {name w h} {
    frame $name -width $w -height $h
    # keep the frame at its requested size (do not let a child resize it)
    pack propagate $name 0
    return $name
}

# ------------------------------------------------------------------ place: exact window coords
proc leg_place {} {
    set top [toplevel .plc]
    wm geometry $top 400x400+0+0
    set f [fixed_frame $top.f 120 80]
    place $f -x 37 -y 52
    update idletasks
    check {[winfo x $f] == 37}       "place: child x != 37"
    check {[winfo y $f] == 52}       "place: child y != 52"
    check {[winfo width $f] == 120}  "place: child width != 120"
    check {[winfo height $f] == 80}  "place: child height != 80"
    # a second child placed relative moves by an exact delta
    set g [fixed_frame $top.g 40 40]
    place $g -x 100 -y 100
    update idletasks
    check {[winfo x $g] == 100 && [winfo y $g] == 100} "place: second child coords wrong"
    destroy $top
}

# ------------------------------------------------------------------ pack: vertical stack at known offsets
proc leg_pack_vstack {} {
    set top [toplevel .pk]
    wm geometry $top 300x400+0+0
    set PAD 6
    set CW 120; set CH 30
    set ys {}
    for {set i 0} {$i < 3} {incr i} {
        set c [fixed_frame $top.c$i $CW $CH]
        pack $c -side top -pady $PAD -anchor w
    }
    update idletasks
    # child i top edge = sum of prior (CH + 2*PAD) + PAD ; each pack pady adds PAD above and below
    for {set i 0} {$i < 3} {incr i} {
        set c $top.c$i
        set ey [expr {$PAD + $i * ($CH + 2 * $PAD)}]
        set gy [winfo y $c]
        check {$gy == $ey} "pack vstack child $i y ($gy) != closed form ($ey)"
        check {[winfo height $c] == $CH} "pack vstack child $i height != $CH"
        check {[winfo width $c] == $CW}  "pack vstack child $i width != $CW"
    }
    destroy $top
}

# ------------------------------------------------------------------ grid: exact row/col/span + cell bbox
proc leg_grid {} {
    set top [toplevel .gd]
    wm geometry $top 400x400+0+0
    set CW 60; set CH 40
    for {set r 0} {$r < 2} {incr r} {
        for {set col 0} {$col < 2} {incr col} {
            set c [fixed_frame $top.c${r}_${col} $CW $CH]
            grid $c -row $r -column $col -padx 0 -pady 0
        }
    }
    update idletasks
    # grid info reports exact placement
    foreach {name er ec} {c0_0 0 0 c0_1 0 1 c1_0 1 0 c1_1 1 1} {
        array set gi [grid info $top.$name]
        check {$gi(-row) == $er}    "grid $name row != $er"
        check {$gi(-column) == $ec} "grid $name column != $ec"
        check {$gi(-rowspan) == 1 && $gi(-columnspan) == 1} "grid $name span != 1x1"
        array unset gi
    }
    # cell (0,0) bbox is a rectangle of at least the child size at the origin
    set bb [grid bbox $top 0 0]
    lassign $bb bx by bw bh
    check {$bx == 0 && $by == 0}          "grid: cell(0,0) origin != (0,0)"
    check {$bw >= $CW && $bh >= $CH}      "grid: cell(0,0) smaller than child"
    # cell (1,1) origin is at the sum of column 0 width / row 0 height (>0)
    set bb2 [grid bbox $top 1 1]
    lassign $bb2 bx2 by2
    check {$bx2 >= $CW && $by2 >= $CH}    "grid: cell(1,1) origin not past first row/col"
    # grid size reports 2x2
    check {[grid size $top] eq {2 2}}     "grid: size != 2x2"
    destroy $top
}

# ------------------------------------------------------------------ labelframe padding composition
proc leg_labelframe {} {
    set top [toplevel .lf]
    wm geometry $top 400x400+0+0
    set PX 12; set PY 8
    set lf [labelframe $top.lf -text hdr -padx $PX -pady $PY -borderwidth 2]
    set inner [label $lf.inner -text inner -width 10 -height 2]
    pack $inner
    pack $lf
    update idletasks
    # the inner label reqwidth is bounded and the labelframe reqwidth exceeds it by at least 2*padx
    set iw [winfo reqwidth $inner]
    set lw [winfo reqwidth $lf]
    check {$iw > 0}                       "labelframe: inner reqwidth is zero"
    check {$lw >= $iw + 2 * $PX}          "labelframe: reqwidth < inner + 2*padx"
    set ih [winfo reqheight $inner]
    set lh [winfo reqheight $lf]
    check {$lh >= $ih + 2 * $PY}          "labelframe: reqheight < inner + 2*pady"
    # the inner label is a child of the labelframe (containment)
    check {[winfo parent $inner] eq $lf}  "labelframe: inner not parented to labelframe"
    destroy $top
}

# ------------------------------------------------------------------ reqwidth/reqheight of a fixed widget
proc leg_reqsize {} {
    set top [toplevel .rs]
    set f [fixed_frame $top.f 77 55]
    update idletasks
    check {[winfo reqwidth $f] == 77}   "reqsize: reqwidth != requested 77"
    check {[winfo reqheight $f] == 55}  "reqsize: reqheight != requested 55"
    destroy $top
}

leg_place
leg_pack_vstack
leg_grid
leg_labelframe
leg_reqsize
gate_finish
