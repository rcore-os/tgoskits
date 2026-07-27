#!/usr/bin/env python3
# MatplotlibCarpet.py - deep closed-form-assertion carpet for Matplotlib on the Agg backend.
#
# Runs entirely headless: MPLBACKEND is forced to Agg before pyplot is imported, so no display,
# GUI toolkit or font-server is ever contacted. Covers the artist/plotting surface (plot / scatter
# / bar / barh / hist / pie / boxplot / step / fill_between / errorbar / imshow / pcolormesh /
# contour), the axes machinery (limits / ticks / labels / legend / title / twinx / axhline /
# axvline / text), the colour stack (viridis+jet sampled values / Normalize / LogNorm / to_rgba /
# to_hex / default cycle) and the render/serialisation path (buffer_rgba pixel values, savefig to
# an in-memory PNG with magic-number + non-empty + byte-for-byte determinism, print_to_buffer).
#
# Every assertion is closed-form: an artist getter, a known count/shape, an exact tick vector, a
# fixed sampled colour, or a rendered pixel driven by a solid patch/facecolor. Floats use a tight
# relative/absolute tolerance; nothing depends on repr, default dtype width or antialiased text, so
# the host reference and a musl target build agree. Self-contained ok/fail counters; prints
# MATPLOTLIB_RESULT then MATPLOTLIB_DONE only when fail == 0.
import io
import math
import os
import sys

# Force the non-interactive Agg backend before pyplot binds a canvas class.
os.environ["MPLBACKEND"] = "Agg"

ok = 0
fail = 0


def chk(name, cond, info=""):
    global ok, fail
    if cond:
        ok += 1
        print("  ok %s%s" % (name, (" " + info) if info else ""))
    else:
        fail += 1
        print("  FAIL %s%s" % (name, (" " + info) if info else ""))


def close(rel, a, b):
    return abs(a - b) <= rel * max(1.0, abs(b))


import numpy as np
import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib import colors as mcolors

chk("version", int(matplotlib.__version__.split(".")[0]) >= 3,
    "matplotlib=%s" % matplotlib.__version__)
chk("backend_agg", matplotlib.get_backend().lower() == "agg",
    "backend=%s" % matplotlib.get_backend())

# ---------------------------------------------------------------- figure / subplots geometry
fig = plt.figure(figsize=(4.0, 3.0), dpi=100)
chk("figure_size_inches", np.allclose(fig.get_size_inches(), [4.0, 3.0]))
chk("figure_dpi", abs(fig.get_dpi() - 100.0) < 1e-9)
plt.close(fig)

fig, ax = plt.subplots(figsize=(2.0, 2.0), dpi=50)
chk("subplots_single_axes", ax is fig.axes[0] and len(fig.axes) == 1)
plt.close(fig)

fig, axs = plt.subplots(2, 3)
chk("subplots_grid_shape", axs.shape == (2, 3))
chk("subplots_grid_count", len(fig.axes) == 6)
plt.close(fig)

fig = plt.figure()
gs_ax = fig.add_subplot(1, 1, 1)
chk("add_subplot", gs_ax in fig.axes)
plt.close(fig)

# ---------------------------------------------------------------- line plot
fig, ax = plt.subplots()
(line,) = ax.plot([0, 1, 2, 3], [0, 1, 4, 9])
chk("plot_xdata", line.get_xdata().tolist() == [0, 1, 2, 3])
chk("plot_ydata", line.get_ydata().tolist() == [0, 1, 4, 9])
# Default property cycle: first line is tab10 blue #1f77b4.
c0 = line.get_color()
blue = mcolors.to_rgb("#1f77b4") if isinstance(c0, str) else (0.12156862745098039,
                                                              0.4666666666666667,
                                                              0.7058823529411765)
chk("plot_default_color", np.allclose(mcolors.to_rgb(c0), blue))
(line2,) = ax.plot([0, 1], [1, 0])
chk("cycle_second_color", np.allclose(mcolors.to_rgb(line2.get_color()),
                                      (1.0, 0.4980392156862745, 0.054901960784313725)))
plt.close(fig)

# ---------------------------------------------------------------- scatter
fig, ax = plt.subplots()
sc = ax.scatter([0, 1, 2], [3, 4, 5])
chk("scatter_offsets", sc.get_offsets().tolist() == [[0.0, 3.0], [1.0, 4.0], [2.0, 5.0]])
chk("scatter_count", sc.get_offsets().shape[0] == 3)
plt.close(fig)

# ---------------------------------------------------------------- bar / barh
fig, ax = plt.subplots()
bars = ax.bar([0, 1, 2], [3, 5, 7])
chk("bar_heights", [b.get_height() for b in bars] == [3, 5, 7])
chk("bar_count", len(bars) == 3)
plt.close(fig)

fig, ax = plt.subplots()
hbars = ax.barh([0, 1, 2], [2, 4, 6])
chk("barh_widths", [b.get_width() for b in hbars] == [2, 4, 6])
plt.close(fig)

# ---------------------------------------------------------------- hist
fig, ax = plt.subplots()
counts, edges, patches = ax.hist(np.array([0.1, 0.2, 0.9, 1.1, 1.9, 2.5]), bins=[0, 1, 2, 3])
chk("hist_counts", counts.tolist() == [3.0, 2.0, 1.0])
chk("hist_edges", edges.tolist() == [0.0, 1.0, 2.0, 3.0])
chk("hist_count_sum", int(counts.sum()) == 6)
chk("hist_npatches", len(patches) == 3)
plt.close(fig)

# ---------------------------------------------------------------- pie
fig, ax = plt.subplots()
wedges, texts = ax.pie([1, 1, 2])
chk("pie_nwedges", len(wedges) == 3)
# Sum of fractions is 4; first wedge (weight 1) spans 90 degrees from 0.
chk("pie_first_wedge_span", abs(wedges[0].theta1 - 0.0) < 1e-9 and
    abs(wedges[0].theta2 - 90.0) < 1e-9)
# Last wedge (weight 2) spans 180 degrees.
chk("pie_last_wedge_span", abs(wedges[2].theta2 - wedges[2].theta1 - 180.0) < 1e-9)
plt.close(fig)

# ---------------------------------------------------------------- boxplot
fig, ax = plt.subplots()
bp = ax.boxplot([np.array([1.0, 2.0, 3.0, 4.0, 5.0])])
chk("boxplot_median", abs(bp["medians"][0].get_ydata()[0] - 3.0) < 1e-9)
chk("boxplot_nmedians", len(bp["medians"]) == 1)
# whiskers of a clean 1..5 sample reach the data extremes.
whisk_y = np.concatenate([w.get_ydata() for w in bp["whiskers"]])
chk("boxplot_whisker_range", abs(whisk_y.min() - 1.0) < 1e-9 and abs(whisk_y.max() - 5.0) < 1e-9)
plt.close(fig)

# ---------------------------------------------------------------- step
fig, ax = plt.subplots()
(sline,) = ax.step([0, 1, 2], [0, 1, 0], where="post")
chk("step_xdata", sline.get_xdata().tolist() == [0, 1, 2])
chk("step_ydata", sline.get_ydata().tolist() == [0, 1, 0])
plt.close(fig)

# ---------------------------------------------------------------- fill_between
fig, ax = plt.subplots()
coll = ax.fill_between([0, 1, 2], [0, 0, 0], [1, 1, 1])
chk("fill_between_npaths", len(coll.get_paths()) == 1)
# The filled polygon's vertical extent is exactly [0, 1].
ext = coll.get_paths()[0].get_extents()
chk("fill_between_extent", abs(ext.y0 - 0.0) < 1e-9 and abs(ext.y1 - 1.0) < 1e-9)
plt.close(fig)

# ---------------------------------------------------------------- errorbar
fig, ax = plt.subplots()
container = ax.errorbar([0, 1, 2], [0, 1, 2], yerr=[0.1, 0.1, 0.1])
chk("errorbar_line", container[0].get_xdata().tolist() == [0, 1, 2])
chk("errorbar_has_caps_or_bars", len(container[1]) >= 0 and len(container[2]) >= 1)
plt.close(fig)

# ---------------------------------------------------------------- imshow
fig, ax = plt.subplots()
img = np.array([[0.0, 1.0], [1.0, 0.0]])
im = ax.imshow(img, cmap="gray", vmin=0.0, vmax=1.0)
chk("imshow_array_shape", im.get_array().shape == (2, 2))
chk("imshow_clim", im.get_clim() == (0.0, 1.0))
chk("imshow_array_values", np.array_equal(np.asarray(im.get_array()), img))
plt.close(fig)

# ---------------------------------------------------------------- pcolormesh
fig, ax = plt.subplots()
qm = ax.pcolormesh(np.array([[1.0, 2.0], [3.0, 4.0]]))
chk("pcolormesh_array", np.asarray(qm.get_array()).ravel().tolist() == [1.0, 2.0, 3.0, 4.0])
plt.close(fig)

# ---------------------------------------------------------------- contour
fig, ax = plt.subplots()
xg = np.linspace(-1.0, 1.0, 11)
yg = np.linspace(-1.0, 1.0, 11)
Xg, Yg = np.meshgrid(xg, yg)
Zg = Xg ** 2 + Yg ** 2
cs = ax.contour(Xg, Yg, Zg, levels=[0.25, 0.5])
chk("contour_levels", cs.levels.tolist() == [0.25, 0.5])
plt.close(fig)

# ---------------------------------------------------------------- axes labels / title / limits / ticks
fig, ax = plt.subplots()
ax.set_xlabel("XL")
ax.set_ylabel("YL")
ax.set_title("TT")
chk("axis_labels", ax.get_xlabel() == "XL" and ax.get_ylabel() == "YL")
chk("axis_title", ax.get_title() == "TT")
ax.set_xlim(0.0, 10.0)
ax.set_ylim(-5.0, 5.0)
chk("axis_xlim", ax.get_xlim() == (0.0, 10.0))
chk("axis_ylim", ax.get_ylim() == (-5.0, 5.0))
ax.set_xticks([0, 5, 10])
ax.set_yticks([-5, 0, 5])
chk("axis_xticks", ax.get_xticks().tolist() == [0, 5, 10])
chk("axis_yticks", ax.get_yticks().tolist() == [-5, 0, 5])
ax.set_xticklabels(["a", "b", "c"])
chk("axis_xticklabels", [t.get_text() for t in ax.get_xticklabels()] == ["a", "b", "c"])
ax.invert_yaxis()
chk("axis_invert", ax.get_ylim() == (5.0, -5.0))
plt.close(fig)

# ---------------------------------------------------------------- legend
fig, ax = plt.subplots()
ax.plot([0, 1], [0, 1], label="A")
ax.plot([0, 1], [1, 0], label="B")
leg = ax.legend()
chk("legend_texts", [t.get_text() for t in leg.get_texts()] == ["A", "B"])
chk("legend_nentries", len(leg.get_texts()) == 2)
plt.close(fig)

# ---------------------------------------------------------------- twinx / axhline / axvline / text
fig, ax = plt.subplots()
ax2 = ax.twinx()
chk("twinx_shares_x", ax2.get_shared_x_axes().joined(ax, ax2))
hl = ax.axhline(y=2.0)
vl = ax.axvline(x=3.0)
chk("axhline", list(hl.get_ydata()) == [2.0, 2.0])
chk("axvline", list(vl.get_xdata()) == [3.0, 3.0])
t = ax.text(0.5, 0.25, "hello")
chk("text_content", t.get_text() == "hello")
chk("text_position", t.get_position() == (0.5, 0.25))
plt.close(fig)

# ---------------------------------------------------------------- colormaps: sampled closed-form values
viridis = matplotlib.colormaps["viridis"]
chk("viridis_0", np.allclose(viridis(0.0), (0.267004, 0.004874, 0.329415, 1.0), atol=1e-6))
chk("viridis_1", np.allclose(viridis(1.0), (0.993248, 0.906157, 0.143936, 1.0), atol=1e-6))
chk("viridis_half", np.allclose(viridis(0.5), (0.127568, 0.566949, 0.550556, 1.0), atol=1e-6))
jet = matplotlib.colormaps["jet"]
chk("jet_0", np.allclose(jet(0.0), (0.0, 0.0, 0.5, 1.0), atol=1e-9))
chk("jet_1", np.allclose(jet(1.0), (0.5, 0.0, 0.0, 1.0), atol=1e-9))
chk("jet_half", np.allclose(jet(0.5), (0.490196, 1.0, 0.477546, 1.0), atol=1e-6))
chk("cmap_N", viridis.N == 256)
# Under/over/bad handling is deterministic at the clamped ends.
chk("cmap_under_clamp", np.allclose(viridis(-1.0), viridis(0.0)))
chk("cmap_over_clamp", np.allclose(viridis(2.0), viridis(1.0)))

# ---------------------------------------------------------------- normalization + colour conversion
n = mcolors.Normalize(vmin=0.0, vmax=10.0)
chk("normalize", abs(float(n(5.0)) - 0.5) < 1e-12 and abs(float(n(0.0))) < 1e-12
    and abs(float(n(10.0)) - 1.0) < 1e-12)
ln = mcolors.LogNorm(vmin=1.0, vmax=100.0)
chk("lognorm", abs(float(ln(10.0)) - 0.5) < 1e-9)
chk("to_rgba", mcolors.to_rgba("red") == (1.0, 0.0, 0.0, 1.0))
chk("to_rgba_alpha", mcolors.to_rgba("black", alpha=0.5) == (0.0, 0.0, 0.0, 0.5))
chk("to_hex", mcolors.to_hex((0.0, 0.0, 1.0)) == "#0000ff")
chk("to_rgb_named", np.allclose(mcolors.to_rgb("white"), (1.0, 1.0, 1.0)))

# ---------------------------------------------------------------- render: buffer_rgba pixel values
# A solid red patch spanning the entire axes -> centre pixel is exactly opaque red.
fig = plt.figure(figsize=(2.0, 2.0), dpi=50)
ax = fig.add_axes([0.0, 0.0, 1.0, 1.0])
ax.set_axis_off()
ax.set_xlim(0.0, 1.0)
ax.set_ylim(0.0, 1.0)
ax.add_patch(plt.Rectangle((0.0, 0.0), 1.0, 1.0, facecolor=(1.0, 0.0, 0.0), edgecolor="none"))
fig.canvas.draw()
buf = np.asarray(fig.canvas.buffer_rgba())
chk("buffer_rgba_shape", buf.shape == (100, 100, 4) and buf.dtype == np.uint8)
chk("buffer_rgba_center_red", buf[50, 50].tolist() == [255, 0, 0, 255])
plt.close(fig)

# Figure facecolor drives the corner pixel deterministically.
fig = plt.figure(figsize=(1.0, 1.0), dpi=50, facecolor="blue")
fig.canvas.draw()
buf = np.asarray(fig.canvas.buffer_rgba())
chk("facecolor_corner_blue", buf[0, 0].tolist() == [0, 0, 255, 255])
plt.close(fig)

fig = plt.figure(figsize=(1.0, 1.0), dpi=50, facecolor="green")
fig.canvas.draw()
buf = np.asarray(fig.canvas.buffer_rgba())
# matplotlib "green" is (0, 0.5, 0) -> 128 after 8-bit quantisation.
chk("facecolor_corner_green", buf[0, 0].tolist() == [0, 128, 0, 255])
plt.close(fig)

# ---------------------------------------------------------------- render: print_to_buffer size + determinism
fig = plt.figure(figsize=(1.0, 1.0), dpi=50, facecolor="white")
b1, (w, h) = fig.canvas.print_to_buffer()
b2, _ = fig.canvas.print_to_buffer()
chk("print_to_buffer_size", w == 50 and h == 50 and len(b1) == 50 * 50 * 4)
chk("print_to_buffer_deterministic", b1 == b2)
plt.close(fig)

# ---------------------------------------------------------------- savefig -> in-memory PNG
fig, ax = plt.subplots(figsize=(2.0, 2.0), dpi=50)
ax.plot([0, 1, 2], [0, 1, 4])
buf1 = io.BytesIO()
fig.savefig(buf1, format="png")
png1 = buf1.getvalue()
buf2 = io.BytesIO()
fig.savefig(buf2, format="png")
png2 = buf2.getvalue()
chk("savefig_png_magic", list(png1[:8]) == [137, 80, 78, 71, 13, 10, 26, 10])
chk("savefig_png_nonempty", len(png1) > 100)
chk("savefig_png_deterministic", png1 == png2)
plt.close(fig)

# savefig to a raw RGBA buffer via the .raw sink round-trips into the same pixel grid.
fig = plt.figure(figsize=(1.0, 1.0), dpi=50, facecolor="red")
raw = io.BytesIO()
fig.savefig(raw, format="raw")
data = np.frombuffer(raw.getvalue(), dtype=np.uint8)
chk("savefig_raw_len", data.size == 50 * 50 * 4)
chk("savefig_raw_red_pixel", data[:4].tolist() == [255, 0, 0, 255])
plt.close(fig)

# ================================================================ SUPPLEMENT (full-API carpet)
import tempfile
import datetime as _dt
import matplotlib.patches as mpatches
import matplotlib.ticker as mticker
import matplotlib.transforms as mtransforms
import matplotlib.scale as mscale
import matplotlib.image as mimage
import matplotlib.dates as mdates
import matplotlib.cm as mcm
from matplotlib.path import Path
from matplotlib.lines import Line2D, lineStyles, lineMarkers
from matplotlib.markers import MarkerStyle
from matplotlib.collections import LineCollection, PolyCollection, QuadMesh, PathCollection
from matplotlib.gridspec import GridSpec, GridSpecFromSubplotSpec
from matplotlib.font_manager import FontProperties, findfont

# ---------------------------------------------------------------- Axes plotting: hist2d / hexbin
fig, ax = plt.subplots()
H, xe, ye, _im = ax.hist2d(np.array([0.5, 0.5, 1.5]), np.array([0.5, 0.5, 1.5]),
                           bins=[[0, 1, 2], [0, 1, 2]])
chk("hist2d_shape", H.shape == (2, 2))
chk("hist2d_sum", int(H.sum()) == 3 and int(H[0, 0]) == 2)
chk("hist2d_edges", xe.tolist() == [0, 1, 2] and ye.tolist() == [0, 1, 2])
plt.close(fig)

fig, ax = plt.subplots()
hb = ax.hexbin([0, 0, 1, 1], [0, 0, 1, 1], gridsize=2)
chk("hexbin_count_sum", int(np.asarray(hb.get_array()).sum()) == 4)
plt.close(fig)

# ---------------------------------------------------------------- violinplot / stackplot / stem
fig, ax = plt.subplots()
vp = ax.violinplot([np.array([1.0, 2, 3, 4, 5]), np.array([2.0, 3, 4, 5, 6])])
chk("violinplot_bodies", len(vp["bodies"]) == 2)
plt.close(fig)

fig, ax = plt.subplots()
scols = ax.stackplot([0, 1, 2], [1, 1, 1], [2, 2, 2])
chk("stackplot_nseries", len(scols) == 2)
plt.close(fig)

fig, ax = plt.subplots()
stemc = ax.stem([0, 1, 2], [1, 2, 3])
chk("stem_markerline_count", len(stemc.markerline.get_xdata()) == 3)
plt.close(fig)

# ---------------------------------------------------------------- quiver / streamplot
fig, ax = plt.subplots()
q = ax.quiver([0, 1], [0, 1], [1, 1], [1, 1])
chk("quiver_offsets_shape", q.get_offsets().shape == (2, 2))
plt.close(fig)

fig, ax = plt.subplots()
_U = np.ones((3, 3)); _V = np.ones((3, 3))
sp = ax.streamplot(np.arange(3), np.arange(3), _U, _V)
chk("streamplot_lines_lc", isinstance(sp.lines, LineCollection))
plt.close(fig)

# ---------------------------------------------------------------- contourf / annotate
fig, ax = plt.subplots()
_xg = np.linspace(-1.0, 1.0, 11)
_Xg, _Yg = np.meshgrid(_xg, _xg)
_Zg = _Xg ** 2 + _Yg ** 2
cf = ax.contourf(_Xg, _Yg, _Zg, levels=[0.0, 0.25, 0.5])
chk("contourf_levels", cf.levels.tolist() == [0.0, 0.25, 0.5])
plt.close(fig)

fig, ax = plt.subplots()
ann = ax.annotate("x", xy=(1, 1), xytext=(2, 2), arrowprops=dict(arrowstyle="->"))
chk("annotate_text", ann.get_text() == "x")
chk("annotate_xy", tuple(ann.xy) == (1, 1))
chk("annotate_xyann", tuple(ann.xyann) == (2, 2))
plt.close(fig)

# ---------------------------------------------------------------- fill_betweenx / axhspan / axvspan
fig, ax = plt.subplots()
fbx = ax.fill_betweenx([0, 1, 2], [0, 0, 0], [1, 1, 1])
chk("fill_betweenx_npaths", len(fbx.get_paths()) == 1)
_e = fbx.get_paths()[0].get_extents()
chk("fill_betweenx_extent", abs(_e.x0 - 0.0) < 1e-9 and abs(_e.x1 - 1.0) < 1e-9)
hs = ax.axhspan(1.0, 2.0)
chk("axhspan_height", isinstance(hs, mpatches.Patch) and
    abs(hs.get_path().get_extents().height - 1.0) < 1e-9)
vs = ax.axvspan(3.0, 4.0)
chk("axvspan_width", abs(vs.get_path().get_extents().width - 1.0) < 1e-9)
plt.close(fig)

# ---------------------------------------------------------------- loglog / semilogx / semilogy / scales
fig, ax = plt.subplots()
ax.loglog([1, 10], [1, 10])
chk("loglog_scales", ax.get_xscale() == "log" and ax.get_yscale() == "log")
plt.close(fig)
fig, ax = plt.subplots()
ax.semilogx([1, 10], [1, 2])
chk("semilogx_scale", ax.get_xscale() == "log" and ax.get_yscale() == "linear")
plt.close(fig)
fig, ax = plt.subplots()
ax.semilogy([1, 2], [1, 10])
chk("semilogy_scale", ax.get_yscale() == "log" and ax.get_xscale() == "linear")
plt.close(fig)
fig, ax = plt.subplots()
ax.set_xscale("symlog")
chk("set_xscale_symlog", ax.get_xscale() == "symlog")
ax.set_xscale("logit")
chk("set_xscale_logit", ax.get_xscale() == "logit")
plt.close(fig)

# ---------------------------------------------------------------- grid / aspect / margins / fmt / eventplot / secondary
fig, ax = plt.subplots()
ax.grid(True)
chk("grid_visible", ax.xaxis.get_gridlines()[0].get_visible() is True)
ax.set_aspect("equal")
chk("aspect_equal", ax.get_aspect() == 1.0)
plt.close(fig)
fig, ax = plt.subplots()
ax.plot([0, 10], [0, 10])
ax.margins(0.1)
chk("margins", ax.margins() == (0.1, 0.1))
plt.close(fig)
fig, ax = plt.subplots()
(fmtline,) = ax.plot([0, 1], [0, 1], "ro--")
chk("fmt_marker", fmtline.get_marker() == "o")
chk("fmt_linestyle", fmtline.get_linestyle() == "--")
chk("fmt_color", fmtline.get_color() == "r")
plt.close(fig)
fig, ax = plt.subplots()
ep = ax.eventplot([1, 2, 3])
chk("eventplot_one_lc", len(ep) == 1 and type(ep[0]).__name__ == "EventCollection")
plt.close(fig)
fig, ax = plt.subplots()
sax = ax.secondary_xaxis("top")
chk("secondary_xaxis", type(sax).__name__ == "SecondaryAxis")
plt.close(fig)

# ---------------------------------------------------------------- colors: colormap construction + norms
lseg = mcolors.LinearSegmentedColormap.from_list("t", ["black", "white"])
# midpoint 0.5 maps to the 128/255 grey (256-entry LUT quantisation).
chk("linseg_from_list_mid", np.allclose(lseg(0.5), (128 / 255, 128 / 255, 128 / 255, 1.0), atol=1e-6))
listedc = mcolors.ListedColormap(["r", "g", "b"])
chk("listed_N", listedc.N == 3)
chk("listed_colors", np.allclose(listedc(0), (1.0, 0.0, 0.0, 1.0)) and
    np.allclose(listedc(1), (0.0, 0.5, 0.0, 1.0)) and
    np.allclose(listedc(2), (0.0, 0.0, 1.0, 1.0)))
bnorm = mcolors.BoundaryNorm([0, 1, 2, 3], 3)
chk("boundary_norm", int(bnorm(1.5)) == 1 and int(bnorm(0.5)) == 0 and int(bnorm(2.5)) == 2)
slog = mcolors.SymLogNorm(linthresh=1.0, vmin=-10.0, vmax=10.0)
chk("symlog_norm_center", abs(float(slog(0.0)) - 0.5) < 1e-9)
pnorm = mcolors.PowerNorm(gamma=2.0, vmin=0.0, vmax=1.0)
chk("power_norm", abs(float(pnorm(0.5)) - 0.25) < 1e-9)
tsn = mcolors.TwoSlopeNorm(vcenter=0.0, vmin=-1.0, vmax=1.0)
chk("twoslope_norm", abs(float(tsn(0.0)) - 0.5) < 1e-9)
cnorm = mcolors.CenteredNorm(vcenter=0.0, halfrange=1.0)
chk("centered_norm", abs(float(cnorm(0.0)) - 0.5) < 1e-9)
nz = mcolors.Normalize(0.0, 10.0)
chk("normalize_inverse", abs(float(nz.inverse(0.5)) - 5.0) < 1e-12)
chk("to_rgba_array_shape", mcolors.to_rgba_array(["r", "g"]).shape == (2, 4))
chk("rgb_to_hsv", np.allclose(mcolors.rgb_to_hsv((1.0, 0.0, 0.0)), (0.0, 1.0, 1.0)))
chk("hsv_to_rgb_roundtrip", np.allclose(mcolors.hsv_to_rgb(mcolors.rgb_to_hsv((0.3, 0.6, 0.9))),
                                        (0.3, 0.6, 0.9)))
chk("is_color_like", mcolors.is_color_like("#abc") is True and mcolors.is_color_like("zzz") is False)
chk("same_color", bool(mcolors.same_color("r", "#ff0000")) and
    not bool(mcolors.same_color("r", "#00ff00")))
chk("cmap_reversed", np.allclose(viridis.reversed()(0.0), viridis(1.0)))
_vir_over = viridis.copy()
_vir_over.set_over("k")
chk("cmap_set_over", np.allclose(_vir_over.get_over(), (0.0, 0.0, 0.0, 1.0)))
_vir_under = viridis.copy()
_vir_under.set_under("w")
chk("cmap_set_under", np.allclose(_vir_under.get_under(), (1.0, 1.0, 1.0, 1.0)))
_vb = viridis(0.5, bytes=True)
_vf = viridis(0.5)
chk("cmap_bytes_dtype", isinstance(_vb[0], np.uint8))
# Byte channels equal 255*float within one quantisation step (int-truncation vs round
# differs by <=1; keep robust across softfloat rounding).
chk("cmap_bytes_value", all(abs(int(_vb[i]) - 255 * _vf[i]) <= 1.0 for i in range(4)))
chk("css4_navy", mcolors.CSS4_COLORS["navy"] == "#000080")
chk("tableau_blue", mcolors.TABLEAU_COLORS["tab:blue"] == "#1f77b4")
chk("base_colors_r", np.allclose(mcolors.to_rgb(mcolors.BASE_COLORS["r"]), (1.0, 0.0, 0.0)))

# ---------------------------------------------------------------- ticker: formatters + locators
chk("func_formatter", mticker.FuncFormatter(lambda x, p: "v%d" % x)(3, 0) == "v3")
chk("formatstr_formatter", mticker.FormatStrFormatter("%.2f")(1.5) == "1.50")
chk("strmethod_formatter", mticker.StrMethodFormatter("{x:.1f}")(2.5) == "2.5")
chk("percent_formatter", mticker.PercentFormatter(xmax=1).format_pct(0.5, 1) == "50%")
chk("eng_formatter", mticker.EngFormatter(unit="Hz")(1000) == "1 kHz")
chk("null_formatter", mticker.NullFormatter().format_data(1.5) == "")
chk("scalar_formatter", type(mticker.ScalarFormatter()).__name__ == "ScalarFormatter")
chk("log_formatter", type(mticker.LogFormatter()).__name__ == "LogFormatter")
chk("multiple_locator", mticker.MultipleLocator(2).tick_values(0, 10).tolist() ==
    [-2.0, 0.0, 2.0, 4.0, 6.0, 8.0, 10.0, 12.0])
chk("fixed_locator", list(mticker.FixedLocator([1, 3, 5]).tick_values(0, 0)) == [1, 3, 5])
chk("maxn_locator", len(mticker.MaxNLocator(3).tick_values(0, 10)) <= 5)
chk("linear_locator", mticker.LinearLocator(5).tick_values(0, 1).tolist() ==
    [0.0, 0.25, 0.5, 0.75, 1.0])
_lv = mticker.LogLocator(base=10).tick_values(1, 1000).tolist()
chk("log_locator", all(v in _lv for v in [1.0, 10.0, 100.0, 1000.0]))
chk("null_locator", list(mticker.NullLocator().tick_values(0, 10)) == [])
chk("auto_minor_locator", type(mticker.AutoMinorLocator()).__name__ == "AutoMinorLocator")

# ---------------------------------------------------------------- patches
r = mpatches.Rectangle((1, 2), 3, 4)
chk("rect_xywh", r.get_x() == 1 and r.get_y() == 2 and r.get_width() == 3 and r.get_height() == 4)
_rb = r.get_bbox()
chk("rect_bbox", _rb.x0 == 1.0 and _rb.y0 == 2.0 and _rb.x1 == 4.0 and _rb.y1 == 6.0)
circ = mpatches.Circle((0, 0), 2)
chk("circle_center_radius", tuple(circ.get_center()) == (0, 0) and circ.get_radius() == 2.0)
chk("circle_contains", circ.contains_point((0, 0)) is True)
ell = mpatches.Ellipse((0, 0), 4, 2, angle=30)
chk("ellipse_wha", ell.width == 4 and ell.height == 2 and ell.angle == 30)
poly = mpatches.Polygon([[0, 0], [2, 0], [1, 2]])
chk("polygon_xy_shape", poly.get_xy().shape[1] == 2 and poly.get_xy().shape[0] >= 3)
wed = mpatches.Wedge((0, 0), 1, 0, 90)
chk("wedge_theta", wed.theta1 == 0 and wed.theta2 == 90)
arc = mpatches.Arc((0, 0), 2, 2, theta1=0, theta2=90)
chk("arc_theta", arc.theta1 == 0 and arc.theta2 == 90)
rpoly = mpatches.RegularPolygon((0, 0), 5)
chk("regularpolygon_nv", rpoly.numvertices == 5)
farrow = mpatches.FancyArrow(0, 0, 1, 0)
chk("fancyarrow_verts", farrow.get_path().vertices.shape[0] >= 3)
fap = mpatches.FancyArrowPatch((0, 0), (1, 1))
chk("fancyarrowpatch", isinstance(fap, mpatches.FancyArrowPatch))
pp = mpatches.PathPatch(Path.unit_rectangle())
chk("pathpatch", isinstance(pp.get_path(), Path))

# ---------------------------------------------------------------- collections / lines / markers
lc = LineCollection([[(0, 0), (1, 1)]])
chk("linecollection_segments", lc.get_segments()[0].tolist() == [[0.0, 0.0], [1.0, 1.0]])
lc.set_array(np.array([0.5]))
chk("linecollection_set_array", np.asarray(lc.get_array()).tolist() == [0.5])
fig, ax = plt.subplots()
pcoll = ax.scatter([0, 1, 2], [0, 1, 2], s=[10, 20, 30])
chk("pathcollection_type", isinstance(pcoll, PathCollection))
chk("pathcollection_sizes", np.asarray(pcoll.get_sizes()).tolist() == [10, 20, 30])
pcoll.set_sizes([5, 5, 5])
chk("pathcollection_set_sizes", np.asarray(pcoll.get_sizes()).tolist() == [5, 5, 5])
chk("collection_facecolors", pcoll.get_facecolors().shape[1] == 4)
plt.close(fig)
fig, ax = plt.subplots()
polyc = ax.fill_between([0, 1, 2], [0, 0, 0], [1, 1, 1])
chk("polycollection_type", isinstance(polyc, PolyCollection))
chk("polycollection_linewidths", np.asarray(polyc.get_linewidths()).tolist() == [1.0])
plt.close(fig)
fig, ax = plt.subplots()
qmesh = ax.pcolormesh(np.array([[1.0, 2.0], [3.0, 4.0]]))
chk("quadmesh_type", isinstance(qmesh, QuadMesh))
chk("quadmesh_array", np.asarray(qmesh.get_array()).ravel().tolist() == [1.0, 2.0, 3.0, 4.0])
plt.close(fig)

l2 = Line2D([0, 1], [2, 3])
chk("line2d_xydata", l2.get_xydata().tolist() == [[0.0, 2.0], [1.0, 3.0]])
chk("line2d_get_data", [list(a) for a in l2.get_data()] == [[0, 1], [2, 3]])
chk("line2d_linestyle_default", l2.get_linestyle() == "-")
l2.set_linestyle("--")
chk("line2d_linestyle_set", l2.get_linestyle() == "--")
chk("line2d_linewidth_default", abs(l2.get_linewidth() - 1.5) < 1e-9)
chk("line2d_get_path_verts", l2.get_path().vertices.shape[0] == 2)
mso = MarkerStyle("o")
chk("markerstyle_o_path", len(mso.get_path().vertices) > 0 and mso.get_fillstyle() == "full")
mss = MarkerStyle("s")
chk("markerstyle_s_path", len(mss.get_path().vertices) == 5)
chk("linestyles_registry", "-" in lineStyles)
chk("linemarkers_registry", "o" in lineMarkers)

# ---------------------------------------------------------------- transforms
bb = mtransforms.Bbox.from_bounds(0, 0, 2, 3)
chk("bbox_from_bounds", bb.width == 2 and bb.height == 3 and bb.x1 == 2 and bb.y1 == 3)
bb2 = mtransforms.Bbox.from_extents(1, 2, 4, 6)
chk("bbox_from_extents", bb2.x0 == 1 and bb2.y0 == 2 and bb2.width == 3 and bb2.height == 4)
_bi = mtransforms.Bbox([[0, 0], [2, 2]]).intersection(
    mtransforms.Bbox([[0, 0], [2, 2]]), mtransforms.Bbox([[1, 1], [3, 3]]))
chk("bbox_intersection", [_bi.x0, _bi.y0, _bi.x1, _bi.y1] == [1.0, 1.0, 2.0, 2.0])
_bu = mtransforms.Bbox.union([mtransforms.Bbox([[0, 0], [2, 2]]),
                              mtransforms.Bbox([[1, 1], [3, 3]])])
chk("bbox_union", [_bu.x0, _bu.y0, _bu.x1, _bu.y1] == [0.0, 0.0, 3.0, 3.0])
chk("bbox_contains", bool(mtransforms.Bbox.from_bounds(0, 0, 2, 2).contains(1, 1)) and
    not bool(mtransforms.Bbox.from_bounds(0, 0, 2, 2).contains(3, 3)))
chk("affine_scale", tuple(mtransforms.Affine2D().scale(2).transform_point((1, 1))) == (2.0, 2.0))
chk("affine_translate", tuple(mtransforms.Affine2D().translate(1, 2).transform_point((0, 0))) == (1.0, 2.0))
chk("affine_rotate90", np.allclose(mtransforms.Affine2D().rotate_deg(90).transform_point((1, 0)),
                                   (0.0, 1.0), atol=1e-12))
chk("identity_transform", tuple(mtransforms.IdentityTransform().transform_point((3, 4))) == (3.0, 4.0))
_aff = mtransforms.Affine2D().scale(2).translate(3, 5)
chk("transform_inverted_roundtrip",
    np.allclose(_aff.inverted().transform_point(_aff.transform_point((5, 7))), (5, 7)))
fig, ax = plt.subplots()
_blend = mtransforms.blended_transform_factory(ax.transData, ax.transAxes)
chk("blended_transform", type(_blend).__name__ == "BlendedGenericTransform")
ax.set_xlim(0.0, 10.0)
ax.set_ylim(0.0, 10.0)
fig.canvas.draw()
_disp = ax.transData.transform((3.0, 4.0))
chk("transData_roundtrip", np.allclose(ax.transData.inverted().transform(_disp), (3.0, 4.0)))
plt.close(fig)

# ---------------------------------------------------------------- gridspec / layout
fig = plt.figure()
gs = GridSpec(2, 3, figure=fig)
chk("gridspec_geometry", gs.get_geometry() == (2, 3))
_ss = gs[0, 1]
chk("subplotspec_num", _ss.num1 == 1 and _ss.num2 == 1)
_pos = _ss.get_position(fig)
chk("subplotspec_position", 0.0 <= _pos.x0 <= 1.0 and 0.0 <= _pos.y1 <= 1.0)
gs_wr = fig.add_gridspec(2, 2, width_ratios=[1, 2])
chk("add_gridspec_width_ratios", list(gs_wr.get_width_ratios()) == [1, 2])
gsf = GridSpecFromSubplotSpec(2, 2, subplot_spec=gs[0, 0])
chk("gridspec_from_subplotspec", gsf.get_geometry() == (2, 2))
plt.close(fig)
fig, ax = plt.subplots()
fig.subplots_adjust(left=0.2)
chk("subplots_adjust", abs(fig.subplotpars.left - 0.2) < 1e-9)
_before = fig.subplotpars.right
fig.tight_layout()
chk("tight_layout_runs", isinstance(fig.subplotpars.right, float))
plt.close(fig)
fig = plt.figure(layout="constrained")
chk("constrained_layout_engine", fig.get_layout_engine() is not None)
plt.close(fig)

# ---------------------------------------------------------------- scale
fig, ax = plt.subplots()
_ls = mscale.LogScale(ax.xaxis)
_lstr = _ls.get_transform()
chk("logscale_forward", abs(float(_lstr.transform(np.array([10.0]))[0]) - 1.0) < 1e-12)
chk("logscale_inverse", abs(float(_lstr.inverted().transform(np.array([1.0]))[0]) - 10.0) < 1e-9)
_syl = mscale.SymmetricalLogScale(ax.xaxis)
chk("symlogscale_forward0", abs(float(_syl.get_transform().transform(np.array([0.0]))[0])) < 1e-12)
_lgt = mscale.LogitScale(ax.xaxis)
chk("logitscale_forward_half", abs(float(_lgt.get_transform().transform(np.array([0.5]))[0])) < 1e-12)
_lin = mscale.LinearScale(ax.xaxis)
chk("linearscale_identity", abs(float(_lin.get_transform().transform(np.array([5.0]))[0]) - 5.0) < 1e-12)
_fsc = mscale.FuncScale(ax.xaxis, (lambda x: x ** 2, lambda x: x ** 0.5))
chk("funcscale_forward", abs(float(_fsc.get_transform().transform(np.array([3.0]))[0]) - 9.0) < 1e-9)
_names = mscale.get_scale_names()
chk("scale_names", all(n in _names for n in ["log", "linear", "logit", "symlog"]))
plt.close(fig)

# ---------------------------------------------------------------- image round-trip
_arr = np.array([[0.0, 0.5], [0.5, 1.0]])
_ibuf = io.BytesIO()
mimage.imsave(_ibuf, _arr, cmap="gray", vmin=0.0, vmax=1.0)
chk("imsave_png_magic", _ibuf.getvalue()[:4] == b"\x89PNG")
_ibuf.seek(0)
_iback = mimage.imread(_ibuf)
chk("imread_shape", _iback.shape == (2, 2, 4))
chk("imread_float_range", _iback.dtype == np.float32 and 0.0 <= _iback.min() and _iback.max() <= 1.0)
fig, ax = plt.subplots()
_axim = ax.imshow(np.zeros((3, 4)))
chk("aximage_extent", list(_axim.get_extent()) == [-0.5, 3.5, 2.5, -0.5])
chk("aximage_get_cmap", _axim.get_cmap().N == 256)
_axim.set_data(np.ones((3, 4)))
chk("aximage_set_data", float(np.asarray(_axim.get_array()).sum()) == 12.0)
plt.close(fig)

# ---------------------------------------------------------------- path
_uc = Path.unit_circle()
chk("path_unit_circle_verts", len(_uc.vertices) == 26)
_ur = Path.unit_rectangle()
_ure = _ur.get_extents()
chk("path_unit_rect_extents", _ure.x0 == 0.0 and _ure.y0 == 0.0 and _ure.x1 == 1.0 and _ure.y1 == 1.0)
_tri = Path([[0, 0], [2, 0], [1, 2], [0, 0]],
            [Path.MOVETO, Path.LINETO, Path.LINETO, Path.CLOSEPOLY])
chk("path_codes", Path.MOVETO == 1 and Path.LINETO == 2 and Path.CLOSEPOLY == 79)
chk("path_contains_point", _tri.contains_point((1, 0.5)) is True)
_te = _tri.get_extents()
chk("path_triangle_extents", _te.x0 == 0.0 and _te.y0 == 0.0 and _te.x1 == 2.0 and _te.y1 == 2.0)
chk("path_contains_points",
    Path.unit_rectangle().contains_points([(0.5, 0.5), (2.0, 2.0)]).tolist() == [True, False])
_cp = Path.make_compound_path(Path.unit_rectangle(), Path.unit_rectangle())
chk("path_make_compound", len(_cp.vertices) == 10)

# ---------------------------------------------------------------- text / font_manager
fig, ax = plt.subplots()
_t = ax.text(0.5, 0.5, "hi")
_t.set_rotation(45)
chk("text_rotation", _t.get_rotation() == 45.0)
chk("text_ha_va_default", _t.get_ha() == "left" and _t.get_va() == "baseline")
_t.set_ha("center")
_t.set_va("top")
chk("text_ha_va_set", _t.get_ha() == "center" and _t.get_va() == "top")
_t.set_fontsize(12)
chk("text_fontsize", _t.get_fontsize() == 12.0)
plt.close(fig)
_fp = FontProperties(size=10, weight="bold", style="italic")
chk("fontproperties_getters", _fp.get_size() == 10.0 and _fp.get_weight() == "bold" and
    _fp.get_style() == "italic")
chk("findfont_nonempty", len(findfont(FontProperties())) > 0)

# ---------------------------------------------------------------- figure OO extras + colorbar + svg/pdf
fig = plt.figure()
_st = fig.suptitle("S")
chk("suptitle", _st.get_text() == "S")
_axp = fig.add_axes([0.1, 0.1, 0.5, 0.5])
_p = _axp.get_position()
chk("add_axes_position", np.allclose([_p.x0, _p.y0, _p.width, _p.height], [0.1, 0.1, 0.5, 0.5]))
fig.set_facecolor("red")
chk("figure_facecolor", fig.get_facecolor() == (1.0, 0.0, 0.0, 1.0))
_n0 = len(fig.axes)
fig.delaxes(_axp)
chk("delaxes", len(fig.axes) == _n0 - 1)
fig.clf()
chk("clf", len(fig.axes) == 0)
plt.close(fig)

fig, ax = plt.subplots()
_im2 = ax.imshow(np.array([[0.0, 1.0], [1.0, 0.0]]))
_cbar = fig.colorbar(_im2)
chk("colorbar_mappable", _cbar.mappable is _im2)
chk("colorbar_ticks", len(_cbar.get_ticks()) > 0)
plt.close(fig)

fig, ax = plt.subplots()
ax.plot([0, 1], [0, 1])
_svg = io.BytesIO()
fig.savefig(_svg, format="svg")
chk("savefig_svg_magic", _svg.getvalue().lstrip()[:5] == b"<?xml" or b"<svg" in _svg.getvalue()[:200])
_pdf = io.BytesIO()
fig.savefig(_pdf, format="pdf")
chk("savefig_pdf_magic", _pdf.getvalue()[:4] == b"%PDF")
plt.close(fig)

# ---------------------------------------------------------------- mpl_toolkits.mplot3d
fig = plt.figure()
ax3 = fig.add_subplot(projection="3d")
chk("axes3d_type", type(ax3).__name__.endswith("Axes3D"))
_sc3 = ax3.scatter([0, 1, 2], [0, 1, 2], [0, 1, 2])
chk("scatter3d_offsets", len(_sc3._offsets3d) == 3 and len(_sc3._offsets3d[0]) == 3)
(_l3,) = ax3.plot([0, 1], [0, 1], [0, 1])
_d3 = _l3.get_data_3d()
chk("plot3d_data", len(_d3) == 3 and len(_d3[0]) == 2)
_X3, _Y3 = np.meshgrid([0, 1], [0, 1])
_Z3 = _X3 + _Y3
_surf = ax3.plot_surface(_X3, _Y3, _Z3)
chk("plot_surface_type", type(_surf).__name__ == "Poly3DCollection")
_wf = ax3.plot_wireframe(_X3, _Y3, _Z3)
chk("plot_wireframe_type", type(_wf).__name__ == "Line3DCollection")
ax3.set_zlim(0.0, 5.0)
chk("set_zlim", ax3.get_zlim() == (0.0, 5.0))
ax3.set_zlabel("Z")
chk("set_zlabel", ax3.get_zlabel() == "Z")
_b3 = ax3.bar3d([0], [0], [0], 1, 1, 1)
chk("bar3d_collection", type(_b3).__name__ == "Poly3DCollection")
plt.close(fig)

# ---------------------------------------------------------------- rcParams / style / cm registry
_saved_lw = matplotlib.rcParams["lines.linewidth"]
matplotlib.rcParams["lines.linewidth"] = 3.0
chk("rcparams_write", matplotlib.rcParams["lines.linewidth"] == 3.0)
with matplotlib.rc_context({"lines.linewidth": 7.0}):
    chk("rc_context_inside", matplotlib.rcParams["lines.linewidth"] == 7.0)
chk("rc_context_restore", matplotlib.rcParams["lines.linewidth"] == 3.0)
plt.rcdefaults()
chk("rcdefaults", matplotlib.rcParams["lines.linewidth"] == 1.5)
chk("style_available", "ggplot" in plt.style.available)
chk("colormaps_len", len(matplotlib.colormaps) > 100 and "viridis" in matplotlib.colormaps)
_sm = mcm.ScalarMappable(mcolors.Normalize(0.0, 1.0), "viridis")
chk("scalarmappable_to_rgba", np.allclose(_sm.to_rgba(0.0), viridis(0.0)))
chk("get_cmap_jet", np.allclose(plt.get_cmap("jet")(0.5), jet(0.5)))

# ---------------------------------------------------------------- matplotlib.dates
_d0 = _dt.datetime(2020, 1, 1)
_num = mdates.date2num(_d0)
_dback = mdates.num2date(_num)
chk("date2num_num2date", _dback.year == 2020 and _dback.month == 1 and _dback.day == 1)
chk("date_formatter", mdates.DateFormatter("%Y")(_num) == "2020")
chk("datestr2num", abs(mdates.datestr2num("2020-01-01") - _num) < 1e-9)
chk("month_year_locators", type(mdates.MonthLocator()).__name__ == "MonthLocator" and
    type(mdates.YearLocator()).__name__ == "YearLocator")
chk("day_locator", type(mdates.DayLocator()).__name__ == "DayLocator")
_dr = mdates.drange(_dt.datetime(2020, 1, 1), _dt.datetime(2020, 1, 4), _dt.timedelta(days=1))
chk("drange_len", len(_dr) == 3)

# ---------------------------------------------------------------- animation (headless: write GIF to temp file)
# HONEST-SKIP: matplotlib.animation to a live display / streaming writers (ffmpeg) is skipped -
# no display and ffmpeg is not guaranteed on-target. Only the pillow file-writer path is exercised.
import matplotlib.animation as manim
chk("animation_pillow_registered", "pillow" in manim.writers.list())
fig, ax = plt.subplots()
(_aline,) = ax.plot([], [])


def _upd(i):
    _aline.set_data([0, i], [0, i])
    return (_aline,)


_anim = manim.FuncAnimation(fig, _upd, frames=3, blit=True)
_fd, _gifpath = tempfile.mkstemp(suffix=".gif")
os.close(_fd)
try:
    _anim.save(_gifpath, writer="pillow")
    with open(_gifpath, "rb") as _gf:
        _gifmagic = _gf.read(4)
    chk("funcanimation_gif_magic", _gifmagic == b"GIF8")
finally:
    if os.path.exists(_gifpath):
        os.remove(_gifpath)
plt.close(fig)
fig, ax = plt.subplots()
_frames = [[ax.plot([0, 1], [0, i])[0]] for i in range(3)]
_aanim = manim.ArtistAnimation(fig, _frames)
chk("artist_animation_constructs", _aanim is not None)
plt.close(fig)

print("MATPLOTLIB_RESULT ok=%d fail=%d" % (ok, fail))
if fail == 0:
    print("MATPLOTLIB_DONE")
    sys.exit(0)
sys.exit(1)
