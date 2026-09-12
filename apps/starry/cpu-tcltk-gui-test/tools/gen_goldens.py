#!/usr/bin/env python3
"""gen_goldens.py - reproduce the closed-form goldens the cpu-tcltk-gui-test cells assert against.

The carpet does not read golden files at runtime: every expected value is computed in-code from first
principles. This tool documents those closed forms in one place so a reviewer can re-derive the exact
constants (the photo fillRect / copy pixel counts, the canvas item coordinates, the pack/grid/place geometry
arithmetic, the fixed-pitch font measure relation) independently of the Tcl source. Run it and compare
against the constants in programs/carpets/gui_*.tcl.
"""


def fillrect_pixels():
    """gui_render leg_fillrect: a 40x30 red rect at (20,15) in a 100x80 photo.
    Interior span [20,60) x [15,45); covered-pixel count = w*h."""
    x0, y0, x1, y1 = 20, 15, 60, 45
    w, h = x1 - x0, y1 - y0
    return {
        "interior_span": ((x0, y0), (x1, y1)),
        "inside_pixel": (x0, y0),          # red
        "one_past_left_edge": (x0 - 1, y0),  # background
        "one_past_right_edge": (x1, y0),     # background
        "red_pixel_count": w * h,          # == 1200
    }


def copy_pixels():
    """gui_render leg_copy_composite: 8x8 red copied to (6,6) over a 30x30 blue photo.
    Opaque copy replaces exactly [6,14) x [6,14)."""
    dst_w, dst_h = 30, 30
    src = 8
    red = src * src                        # == 64
    blue = dst_w * dst_h - red             # == 836
    return {"overlay_span": ((6, 6), (6 + src, 6 + src)),
            "red_pixel_count": red, "blue_pixel_count": blue}


def canvas_items():
    """gui_render leg_canvas_geometry: exact canvas item coords / bbox Tk reports back."""
    return {
        "rectangle_coords": (20.0, 15.0, 60.0, 45.0),
        "rectangle_bbox": (20, 15, 60, 45),
        "oval_coords": (10.0, 10.0, 110.0, 110.0),    # bbox -> center (60,60) r=50
        "line_coords": (10.0, 30.0, 49.0, 30.0),
        "polygon_coords": (0.0, 0.0, 30.0, 0.0, 15.0, 20.0),
        "arc_start_extent": (0.0, 90.0),
        "after_move_5_7": (25.0, 22.0, 65.0, 52.0),   # rectangle moved by (5,7)
    }


def pack_vstack(pad=6, cw=120, ch=30, n=3):
    """gui_layout leg_pack_vstack: child i top edge with pack -pady PAD (adds PAD above and below each)."""
    return [pad + i * (ch + 2 * pad) for i in range(n)]  # -> [6, 48, 90]


def place_coords():
    """gui_layout leg_place: a child placed at -x/-y sits at exactly those winfo x/y."""
    return {"child1": (37, 52), "child2": (100, 100)}


def scale_steps():
    """gui_interact leg_scale: horizontal scale from=0 to=100 resolution=1, start 42.
    Right -> +1, Left -> -1; clamp to [0,100]."""
    return {"start": 42, "after_right": 43, "after_left": 42, "clamp_high": 100, "clamp_low": 0}


def fixed_pitch_measure(one_glyph=12):
    """gui_realassets: for a fixed-pitch family, an N-char string measures exactly N * one-glyph width.
    (The literal one_glyph value depends on the pixel size / font; the RELATION is the golden.)"""
    return {"n5": 5 * one_glyph, "n10": 10 * one_glyph}


if __name__ == "__main__":
    import json
    print("cpu-tcltk-gui-test closed-form goldens (re-derived):")
    print(json.dumps({
        "render.fillrect": fillrect_pixels(),
        "render.copy": copy_pixels(),
        "render.canvas_items": canvas_items(),
        "layout.pack_vstack_y": pack_vstack(),
        "layout.place": place_coords(),
        "interact.scale": scale_steps(),
        "realassets.fixed_pitch_measure_relation": fixed_pitch_measure(),
    }, indent=2, default=str))
