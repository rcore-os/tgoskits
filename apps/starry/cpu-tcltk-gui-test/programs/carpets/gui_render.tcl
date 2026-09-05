# gui_render.tcl - per-pixel photo-image rendering + exact canvas item geometry vs closed form.
#
# Two genuine Tk rendering paths, both asserted against goldens computed from first principles:
#
#   A) photo image (Tk_PhotoImage engine): `$img put color -to x0 y0 x1 y1` fills an exact half-open span,
#      `$img copy` composites one image onto another. We read pixels back with `$img get x y` and assert:
#        - a fillRect's interior pixels are exactly the color, the surrounding background is untouched,
#        - the exact edge pixels (inside vs one-past),
#        - the exact covered-pixel count = w*h,
#        - an opaque `copy` overlay replaces exactly its destination region and nothing else.
#
#   B) canvas (Tk canvas layout engine): create rectangle/oval/line/polygon/arc/text items with known
#      coords, then assert Tk's own `coords` / `bbox` / `itemcget` / `find overlapping` report the exact
#      closed-form geometry. This is Tk's real 2D item model; the numbers are integer-exact and identical
#      across arch. (Grabbing the live canvas to pixels needs the Img `window` format or a Ghostscript
#      rasterizer for `canvas postscript`, neither of which base Tk ships - so pixels come from path A and
#      geometry from path B; together they cover "closed-form pixels" and "closed-form geometry".)
#
# Xvfb provides the X display headlessly. No GPU. Deterministic across arch.

package require Tk
wm withdraw .
source [file join [file dirname [info script]] gui_common.tcl]
gate_init GUI_RENDER

# ------------------------------------------------------------------ A) photo: fillRect exact pixels
proc leg_fillrect {} {
    # 100x80 photo flood-filled dark gray, a 40x30 red rect at (20,15).
    set img [image create photo -width 100 -height 80]
    $img put "#202020" -to 0 0 100 80
    $img put "#ff0000" -to 20 15 60 45   ;# x=[20,60) y=[15,45)

    check [rect_is_color $img 20 15 60 45 255 0 0 0]   "fillRect: interior not exact red"
    # the four background bands around the rect must be untouched dark gray
    check [rect_is_color $img 0 0 100 15 32 32 32 0]    "fillRect: top band disturbed"
    check [rect_is_color $img 0 45 100 80 32 32 32 0]   "fillRect: bottom band disturbed"
    check [rect_is_color $img 0 15 20 45 32 32 32 0]    "fillRect: left band disturbed"
    check [rect_is_color $img 60 15 100 45 32 32 32 0]  "fillRect: right band disturbed"
    # exact edge pixels: one inside is red, one past the edge is background (closed-form half-open span)
    check [pix_is $img 20 15 255 0 0 0]     "fillRect: top-left inside pixel wrong"
    check [pix_is $img 59 44 255 0 0 0]     "fillRect: bottom-right inside pixel wrong"
    check [pix_is $img 19 15 32 32 32 0]    "fillRect: pixel left of edge not bg"
    check [pix_is $img 60 15 32 32 32 0]    "fillRect: pixel right of edge not bg"
    # exact covered-pixel count = 40*30
    check {[count_color $img 0 0 100 80 255 0 0 0] == 1200} "fillRect: red pixel count != 1200"
    image delete $img
}

# ------------------------------------------------------------------ A) photo: opaque copy compositing
proc leg_copy_composite {} {
    # dst: 30x30 blue; src: 8x8 red copied to (6,6). An opaque copy replaces exactly [6,14)x[6,14).
    set dst [image create photo -width 30 -height 30]
    $dst put "#0000ff" -to 0 0 30 30
    set src [image create photo -width 8 -height 8]
    $src put "#ff0000" -to 0 0 8 8
    $dst copy $src -to 6 6

    check [rect_is_color $dst 6 6 14 14 255 0 0 0]  "copy: overlay region not exactly red"
    check [pix_is $dst 6 6 255 0 0 0]               "copy: overlay top-left not red"
    check [pix_is $dst 13 13 255 0 0 0]             "copy: overlay bottom-right not red"
    check [pix_is $dst 5 6 0 0 255 0]               "copy: pixel left of overlay not blue"
    check [pix_is $dst 14 6 0 0 255 0]              "copy: pixel right of overlay not blue"
    check [pix_is $dst 6 5 0 0 255 0]               "copy: pixel above overlay not blue"
    check {[count_color $dst 0 0 30 30 255 0 0 0] == 64} "copy: red pixel count != 8*8"
    check {[count_color $dst 0 0 30 30 0 0 255 0] == 836} "copy: blue pixel count != 900-64"
    image delete $dst; image delete $src
}

# ------------------------------------------------------------------ B) canvas: item geometry closed form
proc leg_canvas_geometry {} {
    set c [canvas .rc -width 130 -height 130 -highlightthickness 0 -borderwidth 0]
    pack $c
    set r [$c create rectangle 20 15 60 45 -fill "#ff0000" -outline ""]
    set o [$c create oval 10 10 110 110 -fill "#0000ff" -outline ""]
    set l [$c create line 10 30 49 30 -fill "#00ff00" -width 1]
    set p [$c create polygon 0 0 30 0 15 20 -fill "#ffff00" -outline ""]
    set a [$c create arc 5 5 45 45 -start 0 -extent 90 -fill "#ff00ff"]
    update idletasks

    # rectangle: exact coords + bbox + type + stored fill color
    check {[$c coords $r] eq {20.0 15.0 60.0 45.0}} "canvas: rectangle coords != closed form"
    check {[$c bbox $r] eq {20 15 60 45}}           "canvas: rectangle bbox != closed form"
    check {[$c type $r] eq "rectangle"}             "canvas: rectangle type wrong"
    check {[$c itemcget $r -fill] eq "#ff0000"}     "canvas: rectangle fill color wrong"
    # oval: coords are the bounding box; center is (60,60), radius 50 (closed form)
    check {[$c coords $o] eq {10.0 10.0 110.0 110.0}} "canvas: oval coords != closed form"
    check {[$c bbox $o] eq {10 10 110 110}}           "canvas: oval bbox != closed form"
    # line: endpoints exact
    check {[$c coords $l] eq {10.0 30.0 49.0 30.0}}   "canvas: line coords != closed form"
    check {[$c type $l] eq "line"}                    "canvas: line type wrong"
    # polygon: three vertices exact
    check {[$c coords $p] eq {0.0 0.0 30.0 0.0 15.0 20.0}} "canvas: polygon coords != closed form"
    check {[$c type $p] eq "polygon"}                 "canvas: polygon type wrong"
    # arc: start/extent exact
    check {[$c itemcget $a -start] eq "0.0"}          "canvas: arc start != 0"
    check {[$c itemcget $a -extent] eq "90.0"}        "canvas: arc extent != 90"
    check {[$c type $a] eq "arc"}                     "canvas: arc type wrong"
    # find overlapping a tiny box inside the rectangle must include the rectangle id
    check {[lsearch [$c find overlapping 25 18 26 19] $r] >= 0} "canvas: find overlapping misses rectangle"
    # a point far outside every item's bbox overlaps nothing
    check {[$c find overlapping 125 125 126 126] eq {}} "canvas: find overlapping hits empty corner"
    # moving the rectangle shifts its coords by an exact delta
    $c move $r 5 7
    check {[$c coords $r] eq {25.0 22.0 65.0 52.0}} "canvas: move did not shift coords by (5,7)"
    destroy $c
}

# ------------------------------------------------------------------ B) canvas: text item bbox (font-agnostic)
proc leg_canvas_text {} {
    set c [canvas .tc -width 60 -height 60 -highlightthickness 0 -borderwidth 0]
    pack $c
    set t [$c create text 20 40 -text "A" -anchor sw -font {Courier -20}]
    update idletasks
    set bb [$c bbox $t]
    check {$bb ne {}} "canvas text: empty bbox"
    if {$bb ne {}} {
        lassign $bb x0 y0 x1 y1
        # anchor sw at (20,40): ink sits above-and-right of the anchor, inside the widget, non-empty extent
        check {$x1 > $x0 && $y1 > $y0}          "canvas text: degenerate bbox"
        check {$x0 >= 10 && $x1 <= 50}          "canvas text: x-extent outside expected band"
        check {$y0 >= 8  && $y1 <= 44}          "canvas text: y-extent outside expected band"
    }
    destroy $c
}

leg_fillrect
leg_copy_composite
leg_canvas_geometry
leg_canvas_text
gate_finish
