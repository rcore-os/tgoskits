#!/usr/bin/env python3
# PandasCarpet.py - deep closed-form-assertion carpet for pandas on musl-native CPython.
#
# Exercises the full pandas surface with deterministic, fixed-seed inputs and exact/closed-form
# expected outputs: DataFrame/Series construction and dtypes; groupby (sum/mean/agg/transform/
# filter); merge/join (inner/outer/left/right); pivot_table/melt/stack/unstack; rolling/expanding/
# ewm windows; resample on a DatetimeIndex; CSV/JSON round-trip through StringIO; loc/iloc/query
# indexing; missing-value handling (fillna/dropna/interpolate); apply/map; concat; sort_values/
# rank; MultiIndex; cut/qcut binning.
#
# Every assertion compares against an exact integer/label result or a closed-form float within a
# tight tolerance; nothing depends on repr, default dtype width or print formatting, so the host
# conda reference and a musl target build agree. Self-contained ok/fail counters; prints
# PANDAS_RESULT then PANDAS_DONE only when fail == 0.
#
# pandas 3.x removed DataFrame/Series.append and DataFrame.applymap; this carpet uses pd.concat
# and DataFrame.map instead so it stays valid on the current API.
import io
import sys

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


import numpy as np
import pandas as pd

chk("version", int(pd.__version__.split(".")[0]) >= 2, "pandas=%s" % pd.__version__)

# ---------------------------------------------------------------- construction / dtype
s = pd.Series([1, 2, 3, 4], name="x")
chk("series_sum", int(s.sum()) == 10)
chk("series_mean", abs(float(s.mean()) - 2.5) < 1e-12)
chk("series_dtype_int", s.dtype == np.dtype("int64"))
chk("series_name", s.name == "x")

df = pd.DataFrame({"a": [1, 2, 3], "b": [4.0, 5.0, 6.0], "c": ["p", "q", "r"]})
chk("df_shape", df.shape == (3, 3))
chk("df_columns", list(df.columns) == ["a", "b", "c"])
chk("df_dtype_a", df["a"].dtype == np.dtype("int64"))
chk("df_dtype_b", df["b"].dtype == np.dtype("float64"))
chk("df_dtype_c", pd.api.types.is_string_dtype(df["c"]))            # object or pandas 3.x str dtype
chk("df_sum_a", int(df["a"].sum()) == 6)
chk("df_values", df[["a", "b"]].to_numpy().tolist() == [[1.0, 4.0], [2.0, 5.0], [3.0, 6.0]])

# from records / dict of rows: exact index and column order.
rec = pd.DataFrame.from_records([{"k": 1, "v": 10}, {"k": 2, "v": 20}])
chk("from_records", rec["v"].tolist() == [10, 20] and list(rec.columns) == ["k", "v"])

# astype round-trip: int -> float -> int is loss-free for small integers.
chk("astype", df["a"].astype("float64").astype("int64").tolist() == [1, 2, 3])

# ---------------------------------------------------------------- groupby
g = pd.DataFrame({
    "key": ["a", "b", "a", "b", "a"],
    "val": [1, 2, 3, 4, 5],
    "w": [10, 20, 30, 40, 50],
})
gs = g.groupby("key")["val"].sum()
chk("groupby_sum", int(gs["a"]) == 9 and int(gs["b"]) == 6)          # a:1+3+5, b:2+4
gm = g.groupby("key")["val"].mean()
chk("groupby_mean", abs(gm["a"] - 3.0) < 1e-12 and abs(gm["b"] - 3.0) < 1e-12)
gc = g.groupby("key").size()
chk("groupby_size", int(gc["a"]) == 3 and int(gc["b"]) == 2)
gag = g.groupby("key").agg(total=("val", "sum"), n=("val", "count"), mx=("w", "max"))
chk("groupby_agg", int(gag.loc["a", "total"]) == 9 and int(gag.loc["a", "n"]) == 3
    and int(gag.loc["a", "mx"]) == 50)
# transform broadcasts the group aggregate back to row shape.
gt = g.groupby("key")["val"].transform("sum")
chk("groupby_transform", gt.tolist() == [9, 6, 9, 6, 9])
# filter keeps only groups whose sum exceeds a threshold (b sums to 6, a to 9).
gf = g.groupby("key").filter(lambda d: d["val"].sum() > 7)
chk("groupby_filter", sorted(gf["key"].unique().tolist()) == ["a"])
# multi-key groupby with two aggregations.
g2 = pd.DataFrame({"x": ["p", "p", "q"], "y": [1, 1, 2], "z": [5, 7, 9]})
g2s = g2.groupby(["x", "y"])["z"].sum()
chk("groupby_multikey", int(g2s.loc[("p", 1)]) == 12 and int(g2s.loc[("q", 2)]) == 9)
# cumulative and first/last group ops.
chk("groupby_cumsum", g.groupby("key")["val"].cumsum().tolist() == [1, 2, 4, 6, 9])
chk("groupby_first", g.groupby("key")["val"].first().tolist() == [1, 2])
chk("groupby_last", g.groupby("key")["val"].last().tolist() == [5, 4])

# ---------------------------------------------------------------- merge / join
left = pd.DataFrame({"k": [1, 2, 3], "lv": ["a", "b", "c"]})
right = pd.DataFrame({"k": [2, 3, 4], "rv": ["x", "y", "z"]})
mi = left.merge(right, on="k", how="inner")
chk("merge_inner", mi["k"].tolist() == [2, 3] and mi["lv"].tolist() == ["b", "c"]
    and mi["rv"].tolist() == ["x", "y"])
ml = left.merge(right, on="k", how="left")
chk("merge_left_keys", ml["k"].tolist() == [1, 2, 3])
chk("merge_left_nan", bool(ml["rv"].isna().tolist() == [True, False, False]))
mr = left.merge(right, on="k", how="right")
chk("merge_right_keys", mr["k"].tolist() == [2, 3, 4])
mo = left.merge(right, on="k", how="outer").sort_values("k").reset_index(drop=True)
chk("merge_outer_keys", mo["k"].tolist() == [1, 2, 3, 4])
chk("merge_outer_na", int(mo["lv"].isna().sum()) == 1 and int(mo["rv"].isna().sum()) == 1)
# index-based join.
jl = pd.DataFrame({"lv": [1, 2]}, index=["a", "b"])
jr = pd.DataFrame({"rv": [3, 4]}, index=["b", "c"])
jj = jl.join(jr, how="inner")
chk("join_index", jj.index.tolist() == ["b"] and int(jj.loc["b", "lv"]) == 2
    and int(jj.loc["b", "rv"]) == 3)

# ---------------------------------------------------------------- pivot / melt / stack
sales = pd.DataFrame({
    "region": ["N", "N", "S", "S"],
    "prod": ["x", "y", "x", "y"],
    "amt": [1, 2, 3, 4],
})
pv = sales.pivot_table(index="region", columns="prod", values="amt", aggfunc="sum")
chk("pivot_table", int(pv.loc["N", "x"]) == 1 and int(pv.loc["S", "y"]) == 4)
chk("pivot_shape", pv.shape == (2, 2))
# pivot_table with a fill and mean aggregation over duplicate cells.
sales2 = pd.concat([sales, pd.DataFrame({"region": ["N"], "prod": ["x"], "amt": [3]})],
                   ignore_index=True)
pv2 = sales2.pivot_table(index="region", columns="prod", values="amt", aggfunc="mean")
chk("pivot_mean_dup", abs(pv2.loc["N", "x"] - 2.0) < 1e-12)          # mean(1, 3)
# melt: wide -> long, exact row count and value recovery.
wide = pd.DataFrame({"id": [1, 2], "m1": [10, 30], "m2": [20, 40]})
mlt = wide.melt(id_vars="id", value_vars=["m1", "m2"], var_name="metric", value_name="v")
chk("melt_rows", mlt.shape == (4, 3))
chk("melt_values", sorted(mlt["v"].tolist()) == [10, 20, 30, 40])
# stack / unstack round-trip on a simple frame.
st = wide.set_index("id").stack()
chk("stack_len", len(st) == 4)
un = st.unstack()
chk("unstack_roundtrip", un.loc[1, "m1"] == 10 and un.loc[2, "m2"] == 40)

# ---------------------------------------------------------------- rolling / expanding / ewm
r = pd.Series([1.0, 2.0, 3.0, 4.0, 5.0])
roll = r.rolling(window=2).sum()
chk("rolling_sum", roll.tolist()[1:] == [3.0, 5.0, 7.0, 9.0] and bool(np.isnan(roll.iloc[0])))
rmean = r.rolling(window=3).mean()
chk("rolling_mean", abs(rmean.iloc[2] - 2.0) < 1e-12 and abs(rmean.iloc[4] - 4.0) < 1e-12)
exp = r.expanding().sum()
chk("expanding_sum", exp.tolist() == [1.0, 3.0, 6.0, 10.0, 15.0])
expm = r.expanding().mean()
chk("expanding_mean", abs(expm.iloc[4] - 3.0) < 1e-12)
# ewm with alpha=0.5: y0=x0; y_t = (x_t + (1-a) y_{t-1}) / (1 + (1-a) + ...). Closed form check.
ew = pd.Series([1.0, 2.0]).ewm(alpha=0.5, adjust=True).mean()
chk("ewm_mean", abs(ew.iloc[0] - 1.0) < 1e-12
    and abs(ew.iloc[1] - (2.0 + 0.5 * 1.0) / (1.0 + 0.5)) < 1e-12)   # (2 + .5*1)/1.5 = 5/3
rstd = pd.Series([1.0, 1.0, 1.0, 1.0]).rolling(2).std()
chk("rolling_std_zero", abs(float(rstd.iloc[3])) < 1e-12)

# ---------------------------------------------------------------- resample (time series)
idx = pd.date_range("2021-01-01", periods=6, freq="D")
ts = pd.Series([1, 2, 3, 4, 5, 6], index=idx)
rs = ts.resample("2D").sum()
chk("resample_2D", rs.tolist() == [3, 7, 11])                        # (1+2),(3+4),(5+6)
rsm = ts.resample("3D").mean()
chk("resample_3D_mean", abs(rsm.iloc[0] - 2.0) < 1e-12 and abs(rsm.iloc[1] - 5.0) < 1e-12)
# upsample + forward fill.
ts2 = pd.Series([10, 20], index=pd.date_range("2021-01-01", periods=2, freq="2D"))
up = ts2.resample("1D").ffill()
chk("resample_ffill", up.tolist() == [10, 10, 20])

# ---------------------------------------------------------------- CSV / JSON round-trip
csv_df = pd.DataFrame({"a": [1, 2, 3], "b": [1.5, 2.5, 3.5], "c": ["u", "v", "w"]})
buf = io.StringIO()
csv_df.to_csv(buf, index=False)
back = pd.read_csv(io.StringIO(buf.getvalue()))
chk("csv_roundtrip_vals", back["a"].tolist() == [1, 2, 3]
    and back["b"].tolist() == [1.5, 2.5, 3.5] and back["c"].tolist() == ["u", "v", "w"])
chk("csv_roundtrip_dtype", back["a"].dtype == np.dtype("int64")
    and back["b"].dtype == np.dtype("float64"))
jbuf = csv_df.to_json(orient="records")
jback = pd.read_json(io.StringIO(jbuf), orient="records")
chk("json_roundtrip", jback["a"].tolist() == [1, 2, 3]
    and jback["c"].tolist() == ["u", "v", "w"])
# JSON split orient preserves index and columns exactly.
jsplit = csv_df.to_json(orient="split")
js = pd.read_json(io.StringIO(jsplit), orient="split")
chk("json_split", list(js.columns) == ["a", "b", "c"] and js.shape == (3, 3))

# ---------------------------------------------------------------- indexing loc/iloc/query
ix = pd.DataFrame({"a": [10, 20, 30, 40], "b": [1, 2, 3, 4]},
                  index=["w", "x", "y", "z"])
chk("loc_scalar", int(ix.loc["y", "a"]) == 30)
chk("loc_slice", ix.loc["x":"y", "a"].tolist() == [20, 30])
chk("iloc_scalar", int(ix.iloc[0, 1]) == 1)
chk("iloc_slice", ix.iloc[1:3]["a"].tolist() == [20, 30])
chk("boolean_mask", ix[ix["a"] > 20]["a"].tolist() == [30, 40])
chk("query", ix.query("a > 20 and b < 4")["a"].tolist() == [30])
chk("at_scalar", int(ix.at["w", "a"]) == 10)
chk("iat_scalar", int(ix.iat[3, 0]) == 40)
chk("isin", ix["a"].isin([20, 40]).tolist() == [False, True, False, True])

# ---------------------------------------------------------------- missing values
nadf = pd.DataFrame({"a": [1.0, np.nan, 3.0, np.nan], "b": [np.nan, 2.0, 3.0, 4.0]})
chk("isna_count", int(nadf.isna().sum().sum()) == 3)
chk("fillna_const", nadf["a"].fillna(0.0).tolist() == [1.0, 0.0, 3.0, 0.0])
chk("fillna_ffill", nadf["a"].ffill().tolist() == [1.0, 1.0, 3.0, 3.0])
chk("fillna_bfill", nadf["b"].bfill().tolist() == [2.0, 2.0, 3.0, 4.0])
dropped = nadf.dropna()
chk("dropna_rows", dropped.index.tolist() == [2])                    # only row 2 is complete
chk("dropna_axis1", nadf.dropna(axis=1).shape[1] == 0)               # both cols have a NaN
interp = pd.Series([0.0, np.nan, np.nan, 3.0]).interpolate()
chk("interpolate_linear", np.allclose(interp.tolist(), [0.0, 1.0, 2.0, 3.0]))
chk("fillna_mean", abs(pd.Series([2.0, np.nan, 4.0]).fillna(
    pd.Series([2.0, np.nan, 4.0]).mean()).iloc[1] - 3.0) < 1e-12)

# ---------------------------------------------------------------- apply / map
ap = pd.DataFrame({"a": [1, 2, 3], "b": [4, 5, 6]})
chk("apply_col_sum", ap.apply(lambda col: col.sum()).tolist() == [6, 15])
chk("apply_row_sum", ap.apply(lambda row: row.sum(), axis=1).tolist() == [5, 7, 9])
chk("series_map", pd.Series([1, 2, 3]).map(lambda v: v * v).tolist() == [1, 4, 9])
chk("series_map_dict", pd.Series(["a", "b", "a"]).map({"a": 0, "b": 1}).tolist() == [0, 1, 0])
chk("df_map_elementwise", ap.map(lambda v: v + 100).to_numpy().tolist()
    == [[101, 104], [102, 105], [103, 106]])
chk("applymap_alias_gone", not hasattr(ap, "applymap"))             # pandas 3.x removed it

# ---------------------------------------------------------------- concat
c1 = pd.DataFrame({"a": [1, 2], "b": [3, 4]})
c2 = pd.DataFrame({"a": [5, 6], "b": [7, 8]})
cc = pd.concat([c1, c2], ignore_index=True)
chk("concat_rows", cc["a"].tolist() == [1, 2, 5, 6] and cc.shape == (4, 2))
ch = pd.concat([c1, c2.rename(columns={"a": "c", "b": "d"})], axis=1)
chk("concat_cols", list(ch.columns) == ["a", "b", "c", "d"] and ch.shape == (2, 4))
# concat with keys builds a MultiIndex.
ck = pd.concat({"first": c1, "second": c2})
chk("concat_keys", ck.index.get_level_values(0).unique().tolist() == ["first", "second"])

# ---------------------------------------------------------------- sort / rank
sv = pd.DataFrame({"a": [3, 1, 2], "b": ["z", "x", "y"]})
srt = sv.sort_values("a")
chk("sort_values", srt["a"].tolist() == [1, 2, 3] and srt["b"].tolist() == ["x", "y", "z"])
srd = sv.sort_values("a", ascending=False)
chk("sort_desc", srd["a"].tolist() == [3, 2, 1])
chk("sort_index", sv.sort_index(ascending=False).index.tolist() == [2, 1, 0])
rk = pd.Series([10, 30, 20]).rank()
chk("rank", rk.tolist() == [1.0, 3.0, 2.0])
rkm = pd.Series([1, 1, 2]).rank(method="min")
chk("rank_min_ties", rkm.tolist() == [1.0, 1.0, 3.0])
chk("nlargest", pd.Series([5, 1, 4, 2, 3]).nlargest(2).tolist() == [5, 4])
chk("nsmallest", pd.Series([5, 1, 4, 2, 3]).nsmallest(2).tolist() == [1, 2])

# ---------------------------------------------------------------- MultiIndex
mi_idx = pd.MultiIndex.from_tuples([("a", 1), ("a", 2), ("b", 1)], names=["g", "n"])
midf = pd.DataFrame({"v": [10, 20, 30]}, index=mi_idx)
chk("multiindex_names", midf.index.names == ["g", "n"])
chk("multiindex_xs", midf.xs("a", level="g")["v"].tolist() == [10, 20])
chk("multiindex_loc", int(midf.loc[("b", 1), "v"]) == 30)
chk("multiindex_sum_level", midf.groupby(level="g")["v"].sum().tolist() == [30, 30])
# set_index / reset_index round-trip.
ri = pd.DataFrame({"g": ["a", "b"], "v": [1, 2]}).set_index("g")
chk("set_index", ri.index.tolist() == ["a", "b"])
chk("reset_index", ri.reset_index()["g"].tolist() == ["a", "b"])

# ---------------------------------------------------------------- cut / qcut
vals = pd.Series([1, 5, 10, 15, 20])
cutb = pd.cut(vals, bins=[0, 10, 20], labels=["lo", "hi"])
chk("cut_labels", cutb.tolist() == ["lo", "lo", "lo", "hi", "hi"])
chk("cut_counts", cutb.value_counts()["lo"] == 3 and cutb.value_counts()["hi"] == 2)
qc = pd.qcut(pd.Series([1, 2, 3, 4]), q=2, labels=["low", "high"])
chk("qcut_labels", qc.tolist() == ["low", "low", "high", "high"])
# value_counts on a categorical distribution is exact.
vc = pd.Series(["a", "b", "a", "a", "c"]).value_counts()
chk("value_counts", int(vc["a"]) == 3 and int(vc["b"]) == 1 and int(vc["c"]) == 1)

# ---------------------------------------------------------------- deterministic sampled aggregate
# Fixed seed makes the whole pipeline reproducible: known counts and a closed-form column sum.
rng = np.random.RandomState(0)
big = pd.DataFrame({
    "grp": rng.randint(0, 3, size=1000),
    "val": rng.randint(0, 100, size=1000),
})
agg = big.groupby("grp")["val"].agg(["count", "sum"])
chk("seeded_total_count", int(agg["count"].sum()) == 1000)
chk("seeded_total_sum", int(agg["sum"].sum()) == int(big["val"].sum()))
chk("seeded_group_partition",
    sorted(agg.index.tolist()) == [0, 1, 2] and int(agg["count"].sum()) == 1000)

# describe() gives exact summary statistics on a known series.
desc = pd.Series([2.0, 4.0, 6.0, 8.0]).describe()
chk("describe_mean", abs(desc["mean"] - 5.0) < 1e-12)
chk("describe_minmax", abs(desc["min"] - 2.0) < 1e-12 and abs(desc["max"] - 8.0) < 1e-12)
chk("describe_median", abs(desc["50%"] - 5.0) < 1e-12)

# crosstab: exact contingency counts.
ct = pd.crosstab(pd.Series(["a", "a", "b"]), pd.Series(["x", "y", "x"]))
chk("crosstab", int(ct.loc["a", "x"]) == 1 and int(ct.loc["a", "y"]) == 1
    and int(ct.loc["b", "x"]) == 1)

# duplicated / drop_duplicates.
dup = pd.DataFrame({"a": [1, 1, 2], "b": [3, 3, 4]})
chk("duplicated", dup.duplicated().tolist() == [False, True, False])
chk("drop_duplicates", dup.drop_duplicates().shape[0] == 2)

print("PANDAS_RESULT ok=%d fail=%d" % (ok, fail))
if fail == 0:
    print("PANDAS_DONE")
    sys.exit(0)
sys.exit(1)
