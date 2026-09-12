#!/usr/bin/env python3
"""gen_goldens.py - reproduce the closed-form goldens the cpu-gui-qt-test cells assert against.

The carpet does not read golden files at runtime: every expected value is computed in-code from first
principles. This tool exists to (a) document those closed forms in one place and (b) let a reviewer
re-derive the exact constants (Porter-Duff "over" result of red@128 over opaque green, the pi*r^2 ellipse
coverage bound, the layout offsets) independently of the C++ source. Run it and compare against the
constants in programs/carpets/gui_*.cpp.
"""
import math


def pd_over(src, dst):
    """Straight-alpha Porter-Duff 'source over destination'. src/dst are (r,g,b,a) 0..255.
    Returns the composited (r,g,b,a) rounded to ints - the exact form QPainter's default
    CompositionMode_SourceOver produces and gui_render.cpp::leg_alpha checks per pixel."""
    sr, sg, sb, sa = src
    dr, dg, db, da = dst
    sa_f, da_f = sa / 255.0, da / 255.0
    oa = sa_f + da_f * (1.0 - sa_f)
    def chan(sc, dc):
        if oa <= 0:
            return 0
        return round((sc * sa_f + dc * da_f * (1.0 - sa_f)) / oa)
    return (chan(sr, dr), chan(sg, dg), chan(sb, db), round(oa * 255.0))


def render_goldens():
    print("== gui_render goldens ==")
    # fillRect(20,15,40,30): exact covered-pixel count
    print(f"fillRect covered pixels = {40*30}  (expect 1200)")
    # drawLine horizontal x=10..49 inclusive, interior sampled 12..47 -> 36 hits
    print(f"drawLine h interior hits (x=12..47) = {47-12+1}  (expect 36)")
    print(f"drawLine v interior hits (y=7..52)  = {52-7+1}  (expect 46)")
    # ellipse: diameter 100 -> r=50, area ~ pi*r^2, tolerance 6%
    r = 50.0
    area = math.pi * r * r
    print(f"ellipse area pi*r^2 = {area:.1f}  (tol 6% -> [{area*0.94:.0f},{area*1.06:.0f}])")
    # Porter-Duff over: red@128 over opaque green
    out = pd_over((255, 0, 0, 128), (0, 255, 0, 255))
    print(f"alpha over red@128 on green(255) = RGBA{out}")


def layout_goldens():
    print("== gui_layout goldens ==")
    # vbox: M=9 S=7 CW=120 CH=30, 3 children
    M, S, CW, CH = 9, 7, 120, 30
    ys = [M + i * (CH + S) for i in range(3)]
    print(f"vbox child y offsets = {ys}  (x={M}, {CW}x{CH})")
    print(f"vbox sizeHint height = {M + 3*CH + 2*S + M}")
    # hbox: M=5 S=11 CW=40 CH=50, 4 children
    M, S, CW, CH = 5, 11, 40, 50
    xs = [M + i * (CW + S) for i in range(4)]
    print(f"hbox child x offsets = {xs}  (y={M}, {CW}x{CH})")
    print(f"hbox sizeHint width  = {M + 4*CW + 3*S + M}")
    # grid: M=8 HS=6 VS=10 CW=60 CH=40, 2x2
    M, HS, VS, CW, CH = 8, 6, 10, 60, 40
    cells = {(r, c): (M + c*(CW+HS), M + r*(CH+VS)) for r in range(2) for c in range(2)}
    print(f"grid cell origins = {cells}")
    print(f"grid sizeHint = {(M + 2*CW + HS + M, M + 2*CH + VS + M)}")
    # resize stretch: host 200x300, M=10 S=8, two equal expanders
    M, S = 10, 8
    usable = 300 - 2*M - S
    print(f"resize(200x300) usable height = {usable}, half = {usable//2}, fill width = {200-2*M}")
    usable2 = 500 - 2*M - S
    print(f"resize(200x500) usable height = {usable2}")


def interact_goldens():
    print("== gui_interact goldens ==")
    print("button: click count 1 then 2; disabled click -> 0")
    print("checkbox: unchecked -> click -> checked -> click -> unchecked; indicator pixels differ")
    print("lineedit: keyClicks 'hello'; backspace -> 'hell'; ctrl-a + 'world' -> 'world'")
    # slider: range 0..100, single=5 page=20, start 50
    v = 50
    v += 5; print(f"slider Key_Right -> {v}")
    v -= 5; print(f"slider Key_Left  -> {v}")
    v += 20; print(f"slider PageUp    -> {v}")
    print("slider Home -> 0 ; End -> 100")


if __name__ == "__main__":
    render_goldens()
    print()
    layout_goldens()
    print()
    interact_goldens()
