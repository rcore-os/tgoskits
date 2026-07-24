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

print("MATPLOTLIB_RESULT ok=%d fail=%d" % (ok, fail))
if fail == 0:
    print("MATPLOTLIB_DONE")
    sys.exit(0)
sys.exit(1)
