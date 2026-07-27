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

# ================================================================ FULL-API SUPPLEMENT
# All expected values below were cross-checked against a real pandas 3.x reference; each is a
# closed-form / documented-behavior result (never a repr- or formatting-dependent value).

# ---------------------------------------------------------------- timeseries: to_datetime / dt
dti = pd.to_datetime(["2021-01-01", "2021-06-15"])
chk("to_datetime_year", dti.year.tolist() == [2021, 2021])
chk("to_datetime_month", dti.month.tolist() == [1, 6])
tsf = pd.to_datetime("20210101", format="%Y%m%d")
chk("to_datetime_format", tsf.year == 2021 and tsf.month == 1 and tsf.day == 1)
chk("to_datetime_unit", pd.to_datetime(0, unit="s") == pd.Timestamp("1970-01-01"))
chk("to_datetime_errors_coerce",
    pd.to_datetime(["2021-01-01", "not-a-date"], errors="coerce").isna().tolist() == [False, True])
# a date_range starting on a Monday: dayofweek runs 0(Mon),1,2,3,4.
dts = pd.Series(pd.date_range("2021-01-04", periods=5, freq="D"))
chk("dt_dayofweek", dts.dt.dayofweek.tolist() == [0, 1, 2, 3, 4])
chk("dt_day", dts.dt.day.tolist() == [4, 5, 6, 7, 8])
chk("dt_month", dts.dt.month.tolist() == [1, 1, 1, 1, 1])
chk("dt_year", dts.dt.year.tolist() == [2021] * 5)
chk("dt_quarter", dts.dt.quarter.tolist() == [1, 1, 1, 1, 1])
chk("dt_is_month_end",
    pd.Series(pd.to_datetime(["2021-01-31", "2021-02-15"])).dt.is_month_end.tolist() == [True, False])
chk("dt_hour",
    pd.Series(pd.to_datetime(["2021-01-01 05:00", "2021-01-01 09:00"])).dt.hour.tolist() == [5, 9])
chk("dt_strftime",
    pd.Series(pd.to_datetime(["2021-01-15", "2021-06-20"])).dt.strftime("%Y-%m").tolist()
    == ["2021-01", "2021-06"])
# Timedelta: 2 days + 3 hours = 2*86400 + 3*3600 = 183600 seconds.
tdv = pd.Timedelta("2 days 3:00:00")
chk("timedelta_total_seconds", abs(tdv.total_seconds() - 183600.0) < 1e-9)
chk("timedelta_days", tdv.days == 2)
chk("timedelta_arith",
    (pd.to_datetime("2021-01-10") - pd.to_datetime("2021-01-01")).days == 9)
# Period: January 2021 asfreq to daily end is the 31st; period_range Jan..Mar has length 3.
per = pd.Period("2021-01", freq="M")
chk("period_asfreq_end", per.asfreq("D", "end").day == 31)
chk("period_range_len", len(pd.period_range("2021-01", "2021-03", freq="M")) == 3)
chk("period_index_type",
    isinstance(pd.period_range("2021-01", periods=2, freq="M"), pd.PeriodIndex))
chk("series_shift", pd.Series([1, 2, 3]).shift(1).iloc[1:].tolist() == [1.0, 2.0]
    and bool(np.isnan(pd.Series([1, 2, 3]).shift(1).iloc[0])))
chk("series_diff_ts", pd.Series([1, 3, 6]).diff().iloc[1:].tolist() == [2.0, 3.0])
# pct_change: 110/100-1 = 0.1, 121/110-1 = 0.1.
pcc = pd.Series([100.0, 110.0, 121.0]).pct_change()
chk("series_pct_change", abs(pcc.iloc[1] - 0.1) < 1e-9 and abs(pcc.iloc[2] - 0.1) < 1e-9)
# date_range with assorted freqs: month-end days for Jan/Feb/Mar 2021 = 31/28/31.
chk("date_range_ME", pd.date_range("2021-01-01", periods=3, freq="ME").day.tolist() == [31, 28, 31])
chk("date_range_W", len(pd.date_range("2021-01-01", periods=3, freq="W")) == 3)
chk("date_range_h", len(pd.date_range("2021-01-01", periods=3, freq="h")) == 3)
chk("date_range_min", len(pd.date_range("2021-01-01", periods=3, freq="min")) == 3)
# business days: 2021-01-01 is Friday(4), then skips weekend to Mon(0), Tue(1).
chk("date_range_B", pd.date_range("2021-01-01", periods=3, freq="B").dayofweek.tolist() == [4, 0, 1])
tstime = pd.Series(pd.to_datetime(["2021-01-01 05:30:00"]))
chk("dt_floor", tstime.dt.floor("D").iloc[0] == pd.Timestamp("2021-01-01"))
chk("dt_normalize", tstime.dt.normalize().iloc[0] == pd.Timestamp("2021-01-01"))
chk("dt_ceil", tstime.dt.ceil("h").iloc[0] == pd.Timestamp("2021-01-01 06:00:00"))
chk("dt_round",
    pd.Series(pd.to_datetime(["2021-01-01 05:20:00"])).dt.round("h").iloc[0]
    == pd.Timestamp("2021-01-01 05:00:00"))
# tz: localize to UTC only (named tz needs a zoneinfo DB that may be absent on target).
tzl = pd.Series(pd.to_datetime(["2021-01-01"])).dt.tz_localize("UTC")
chk("dt_tz_localize", tzl.dt.tz is not None and str(tzl.dt.tz) == "UTC")
chk("dt_tz_convert", str(tzl.dt.tz_convert("UTC").dt.tz) == "UTC")
tstamp = pd.Timestamp("2021-03-14 15:09:26")
chk("timestamp_attrs",
    (tstamp.year, tstamp.month, tstamp.day, tstamp.hour, tstamp.minute) == (2021, 3, 14, 15, 9))

# ---------------------------------------------------------------- string accessor (.str)
chk("str_upper", pd.Series(["aB", "cd"]).str.upper().tolist() == ["AB", "CD"])
chk("str_lower", pd.Series(["aB", "CD"]).str.lower().tolist() == ["ab", "cd"])
chk("str_title", pd.Series(["hello world"]).str.title().tolist() == ["Hello World"])
chk("str_capitalize", pd.Series(["hELLO"]).str.capitalize().tolist() == ["Hello"])
chk("str_swapcase", pd.Series(["aB", "Cd"]).str.swapcase().tolist() == ["Ab", "cD"])
chk("str_len", pd.Series(["a", "bb", "ccc"]).str.len().tolist() == [1, 2, 3])
chk("str_strip", pd.Series(["  x  ", "\ty\t"]).str.strip().tolist() == ["x", "y"])
chk("str_lstrip", pd.Series(["  x"]).str.lstrip().tolist() == ["x"])
chk("str_rstrip", pd.Series(["x  "]).str.rstrip().tolist() == ["x"])
chk("str_pad", pd.Series(["a"]).str.pad(3, fillchar="*").tolist() == ["**a"])
chk("str_zfill", pd.Series(["5"]).str.zfill(3).tolist() == ["005"])
chk("str_center", pd.Series(["a"]).str.center(5, "-").tolist() == ["--a--"])
chk("str_contains", pd.Series(["abc", "xyz"]).str.contains("a").tolist() == [True, False])
chk("str_startswith", pd.Series(["abc", "xbc"]).str.startswith("a").tolist() == [True, False])
chk("str_endswith", pd.Series(["abc", "abx"]).str.endswith("c").tolist() == [True, False])
chk("str_match", pd.Series(["abc", "1bc"]).str.match(r"[a-z]").tolist() == [True, False])
chk("str_fullmatch", pd.Series(["abc", "abcd"]).str.fullmatch(r"abc").tolist() == [True, False])
chk("str_replace_literal", pd.Series(["aaa"]).str.replace("a", "X").tolist() == ["XXX"])
chk("str_replace_regex",
    pd.Series(["a12b"]).str.replace(r"\d+", "N", regex=True).tolist() == ["aNb"])
chk("str_split", pd.Series(["a-b-c"]).str.split("-").tolist() == [["a", "b", "c"]])
chk("str_split_get", pd.Series(["a-b"]).str.split("-").str.get(0).tolist() == ["a"])
chk("str_split_expand", pd.Series(["a-b"]).str.split("-", expand=True).shape == (1, 2))
chk("str_rsplit", pd.Series(["a-b-c"]).str.rsplit("-", n=1).tolist() == [["a-b", "c"]])
chk("str_partition", pd.Series(["a-b-c"]).str.partition("-").values.tolist() == [["a", "-", "b-c"]])
chk("str_cat", pd.Series(["a", "b", "c"]).str.cat(sep="-") == "a-b-c")
chk("str_extract", pd.Series(["a12", "b34"]).str.extract(r"(\d+)")[0].tolist() == ["12", "34"])
chk("str_extractall", len(pd.Series(["a1b2"]).str.extractall(r"(\d)")) == 2)
chk("str_findall", pd.Series(["a1b2"]).str.findall(r"\d").tolist() == [["1", "2"]])
chk("str_slice", pd.Series(["abcd"]).str.slice(0, 2).tolist() == ["ab"])
chk("str_slice_replace", pd.Series(["abcd"]).str.slice_replace(0, 2, "XY").tolist() == ["XYcd"])
chk("str_count", pd.Series(["aaa"]).str.count("a").tolist() == [3])
chk("str_find", pd.Series(["abc"]).str.find("c").tolist() == [2])
gdum = pd.Series(["a|b", "a"]).str.get_dummies(sep="|")
chk("str_get_dummies",
    gdum["a"].tolist() == [1, 1] and gdum["b"].tolist() == [1, 0])
chk("str_isdigit", pd.Series(["123", "a1"]).str.isdigit().tolist() == [True, False])
chk("str_isalpha", pd.Series(["abc", "a1"]).str.isalpha().tolist() == [True, False])
chk("str_isnumeric", pd.Series(["123", "abc"]).str.isnumeric().tolist() == [True, False])
chk("str_isspace", pd.Series(["   ", "a"]).str.isspace().tolist() == [True, False])
chk("str_repeat", pd.Series(["ab"]).str.repeat(2).tolist() == ["abab"])
chk("str_wrap", pd.Series(["a b c d"]).str.wrap(3).tolist() == ["a b\nc d"])
chk("str_normalize", pd.Series(["abc"]).str.normalize("NFC").tolist() == ["abc"])

# ---------------------------------------------------------------- reshape (pivot/dummies/explode)
pvt = pd.DataFrame({"i": [1, 1, 2, 2], "c": ["x", "y", "x", "y"], "v": [10, 20, 30, 40]}) \
    .pivot(index="i", columns="c", values="v")
chk("pivot_nonagg", int(pvt.loc[1, "x"]) == 10 and int(pvt.loc[2, "y"]) == 40)
gdo = pd.get_dummies(pd.Series(["a", "b", "a"]))
chk("get_dummies", list(gdo.columns) == ["a", "b"] and gdo["a"].tolist() == [True, False, True])
chk("from_dummies", pd.from_dummies(gdo).iloc[:, 0].tolist() == ["a", "b", "a"])
w2l = pd.wide_to_long(pd.DataFrame({"id": [1, 2], "A1": [10, 30], "A2": [20, 40]}),
                      stubnames="A", i="id", j="year")
chk("wide_to_long", w2l.shape[0] == 4 and sorted(w2l["A"].tolist()) == [10, 20, 30, 40])
chk("explode", pd.Series([[1, 2], [3]]).explode().tolist() == [1, 2, 3])
fcodes, funiques = pd.factorize(pd.Series(["a", "b", "a"]))
chk("factorize", fcodes.tolist() == [0, 1, 0] and funiques.tolist() == ["a", "b"])
# merge_asof backward: each right-frame time matches the last left key <= it (1->a, 5->b, 10->c).
asof = pd.merge_asof(pd.DataFrame({"t": [2, 6, 11], "rv": ["x", "y", "z"]}),
                     pd.DataFrame({"t": [1, 5, 10], "lv": ["a", "b", "c"]}), on="t")
chk("merge_asof", asof["lv"].tolist() == ["a", "b", "c"])
chk("combine_first",
    pd.DataFrame({"a": [1.0, np.nan]}).combine_first(pd.DataFrame({"a": [100.0, 200.0]}))["a"].tolist()
    == [1.0, 200.0])
mind = pd.DataFrame({"k": [1, 2, 3]}).merge(pd.DataFrame({"k": [2, 3, 4]}), on="k",
                                            how="outer", indicator=True)
mvc = mind["_merge"].value_counts()
chk("merge_indicator",
    int(mvc["both"]) == 2 and int(mvc["left_only"]) == 1 and int(mvc["right_only"]) == 1)
chk("merge_validate",
    pd.DataFrame({"k": [1, 2]}).merge(pd.DataFrame({"k": [1, 2], "v": [9, 8]}),
                                      on="k", validate="one_to_one")["v"].tolist() == [9, 8])
cji = pd.concat([pd.DataFrame({"a": [1], "b": [2]}), pd.DataFrame({"a": [3], "c": [4]})], join="inner")
chk("concat_join_inner", list(cji.columns) == ["a"])
tdf = pd.DataFrame({"a": [1, 2], "b": [3, 4]})
chk("transpose", tdf.T.shape == (2, 2) and tdf.T.T.equals(tdf))

# ---------------------------------------------------------------- categorical dtype
from pandas.api.types import CategoricalDtype
catv = pd.Categorical(["a", "b", "a"])
chk("categorical_codes", catv.codes.tolist() == [0, 1, 0])
chk("categorical_categories", catv.categories.tolist() == ["a", "b"])
scat = pd.Series(["x", "y", "x"]).astype("category")
chk("cat_codes", scat.cat.codes.tolist() == [0, 1, 0])
chk("cat_categories", scat.cat.categories.tolist() == ["x", "y"])
chk("cat_add_categories", scat.cat.add_categories(["z"]).cat.categories.tolist() == ["x", "y", "z"])
chk("cat_rename_categories",
    pd.Series(["a", "b"]).astype("category").cat.rename_categories({"a": "A", "b": "B"}).tolist()
    == ["A", "B"])
chk("cat_reorder_categories",
    pd.Series(["a", "b"]).astype("category").cat.reorder_categories(["b", "a"]).cat.categories.tolist()
    == ["b", "a"])
# ordered categorical: 'a' < 'b' element-wise is True.
ordc = pd.Series(["a", "b", "a"]).astype(CategoricalDtype(["a", "b"], ordered=True))
chk("cat_ordered_compare", (ordc < "b").tolist() == [True, False, True])
chk("cat_as_ordered", pd.Series(["a"]).astype("category").cat.as_ordered().cat.ordered)
chk("cat_remove_unused",
    pd.Series(["a", "a"]).astype(CategoricalDtype(["a", "b"])).cat.remove_unused_categories()
    .cat.categories.tolist() == ["a"])

# ---------------------------------------------------------------- stats / correlation / windowed
corrdf = pd.DataFrame({"x": [1.0, 2.0, 3.0, 4.0], "y": [2.0, 4.0, 6.0, 8.0]})
chk("corr_perfect", abs(corrdf.corr().loc["x", "y"] - 1.0) < 1e-9)         # y = 2x
chk("corr_negative",
    abs(pd.Series([1.0, 2.0, 3.0]).corr(pd.Series([-1.0, -2.0, -3.0])) - (-1.0)) < 1e-9)
chk("series_cov", abs(pd.Series([1.0, 2.0, 3.0]).cov(pd.Series([1.0, 2.0, 3.0])) - 1.0) < 1e-9)
chk("df_cov", abs(corrdf.cov().loc["x", "x"] - (5.0 / 3.0)) < 1e-9)        # var of 1..4, ddof=1
chk("quantile_median", abs(pd.Series([1, 2, 3, 4]).quantile(0.5) - 2.5) < 1e-9)
chk("quantile_q25", abs(pd.Series([1, 2, 3, 4]).quantile(0.25) - 1.75) < 1e-9)  # linear interp
chk("rank_dense", pd.Series([1, 1, 2]).rank(method="dense").tolist() == [1.0, 1.0, 2.0])
chk("rank_first", pd.Series([1, 1, 2]).rank(method="first").tolist() == [1.0, 2.0, 3.0])
chk("rank_max", pd.Series([1, 1, 2]).rank(method="max").tolist() == [2.0, 2.0, 3.0])
chk("rank_pct", pd.Series([10, 20, 30, 40]).rank(pct=True).tolist() == [0.25, 0.5, 0.75, 1.0])
chk("mode", pd.Series([1, 2, 2, 3]).mode().tolist() == [2])
chk("nunique", pd.Series([1, 1, 2, 3]).nunique() == 3)
chk("df_nunique", pd.DataFrame({"a": [1, 1, 2], "b": [1, 2, 3]}).nunique().tolist() == [2, 3])
chk("cumprod", pd.Series([1, 2, 3, 4]).cumprod().tolist() == [1, 2, 6, 24])
chk("cummax", pd.Series([1, 3, 2, 5]).cummax().tolist() == [1, 3, 3, 5])
chk("cummin", pd.Series([5, 3, 4, 1]).cummin().tolist() == [5, 3, 3, 1])
chk("clip", pd.Series([1, 5, 10]).clip(2, 8).tolist() == [2, 5, 8])
chk("abs", pd.Series([-1, -2, 3]).abs().tolist() == [1, 2, 3])
chk("round", pd.Series([1.234, 5.678]).round(1).tolist() == [1.2, 5.7])
chk("var", abs(pd.Series([1.0, 2.0, 3.0, 4.0]).var() - (5.0 / 3.0)) < 1e-9)
chk("std", abs(pd.Series([1.0, 2.0, 3.0, 4.0]).std() - np.sqrt(5.0 / 3.0)) < 1e-9)
chk("median", abs(pd.Series([1.0, 2.0, 3.0, 4.0]).median() - 2.5) < 1e-9)
chk("sem", abs(pd.Series([1.0, 2.0, 3.0, 4.0]).sem() - np.sqrt(5.0 / 3.0) / 2.0) < 1e-9)
chk("skew_symmetric", abs(pd.Series([1.0, 2.0, 3.0, 4.0]).skew()) < 1e-9)  # symmetric -> 0
chk("kurt", abs(pd.Series([1.0, 2.0, 3.0, 4.0]).kurt() - (-1.2)) < 1e-9)
chk("autocorr", abs(pd.Series([1.0, 2.0, 3.0, 4.0]).autocorr() - 1.0) < 1e-9)
gwin = pd.DataFrame({"k": ["a", "a", "a", "b", "b"], "v": [1.0, 2.0, 3.0, 10.0, 20.0]})
chk("groupby_rolling", gwin.groupby("k")["v"].rolling(2).sum().dropna().tolist() == [3.0, 5.0, 30.0])
chk("groupby_rank", gwin.groupby("k")["v"].rank().tolist() == [1.0, 2.0, 3.0, 1.0, 2.0])
chk("groupby_nth", gwin.groupby("k")["v"].nth(0).tolist() == [1.0, 10.0])
chk("groupby_expanding",
    gwin.groupby("k")["v"].expanding().sum().tolist() == [1.0, 3.0, 6.0, 10.0, 30.0])
chk("groupby_apply", gwin.groupby("k")["v"].apply(lambda s: s.max() - s.min()).tolist() == [2.0, 10.0])
chk("value_counts_normalize",
    abs(pd.Series(["a", "a", "b"]).value_counts(normalize=True).sum() - 1.0) < 1e-9)
chk("value_counts_bins", int(pd.Series([1, 2, 3, 4]).value_counts(bins=2).sum()) == 4)

# ---------------------------------------------------------------- indexing: where/mask/eval/assign
sw = pd.Series([1, 2, 3, 4])
chk("where", sw.where(sw > 2, -1).tolist() == [-1, -1, 3, 4])
chk("mask", sw.mask(sw > 2, -1).tolist() == [1, 2, -1, -1])
edf = pd.DataFrame({"a": [1, 2, 3], "b": [4, 5, 6]})
chk("eval", edf.eval("a + b").tolist() == [5, 7, 9])
chk("assign", edf.assign(c=lambda d: d.a + d.b)["c"].tolist() == [5, 7, 9])
chk("pipe", sw.pipe(lambda x: x * 2).tolist() == [2, 4, 6, 8])
chk("between", sw.between(2, 4).tolist() == [False, True, True, True])
fcol = pd.DataFrame({"col_a": [1], "col_b": [2], "row_a": [3]})
chk("filter_like", list(fcol.filter(like="col").columns) == ["col_a", "col_b"])
chk("filter_regex", list(fcol.filter(regex="_a$").columns) == ["col_a", "row_a"])
chk("reindex", pd.DataFrame({"v": [1, 2]}, index=["a", "b"]).reindex(["a", "b", "c"])["v"]
    .isna().tolist() == [False, False, True])
chk("searchsorted", int(pd.Series([1, 3, 5, 7]).searchsorted(4)) == 2)
chk("take", pd.DataFrame({"v": [10, 20, 30]}).take([0, 2])["v"].tolist() == [10, 30])
xsdf = pd.DataFrame({("A", "x"): [1], ("A", "y"): [2], ("B", "x"): [3]})
chk("xs_axis1", xsdf.xs("x", axis=1, level=1).columns.tolist() == ["A", "B"])

# ---------------------------------------------------------------- dtype conversions & nullable
chk("nullable_Int64_sum", pd.Series([1, None, 3], dtype="Int64").sum() == 4)   # skipna
chk("nullable_Int64_isna", pd.Series([1, None, 3], dtype="Int64").isna().tolist() == [False, True, False])
chk("convert_dtypes_int", isinstance(pd.Series([1, 2, 3]).convert_dtypes().dtype, pd.Int64Dtype))
chk("convert_dtypes_string",
    pd.api.types.is_string_dtype(pd.Series(["a", "b"]).convert_dtypes()))
chk("pd_array_Int64", pd.array([1, 2], dtype="Int64").sum() == 3)
chk("astype_boolean", pd.Series([1, 0, 1]).astype("boolean").tolist() == [True, False, True])
chk("isna_NA", pd.isna(pd.NA))
chk("astype_string", pd.api.types.is_string_dtype(pd.Series(["a", "b"]).astype("string")))
# Kleene logic: True & NA -> NA is masked; here True&True, False&True, NA&True -> True,False,NA.
kand = pd.array([True, False, None], dtype="boolean") & pd.array([True, True, True], dtype="boolean")
chk("kleene_and", kand[0] is True or kand[0] == True)
chk("kleene_and_na", pd.isna(kand[2]) and (kand[1] == False))
kor = pd.array([True, False, None], dtype="boolean") | pd.array([True, True, True], dtype="boolean")
chk("kleene_or", (kor[0] == True) and (kor[1] == True) and (kor[2] == True))  # X | True == True
chk("infer_objects", pd.Series([1, 2, 3], dtype=object).infer_objects().dtype == np.dtype("int64"))

# ---------------------------------------------------------------- IO: to_dict/records/html/fwf/table
iodf = pd.DataFrame({"a": [1, 2], "b": [3, 4]})
chk("to_dict_records", iodf.to_dict(orient="records") == [{"a": 1, "b": 3}, {"a": 2, "b": 4}])
chk("to_dict_list", iodf.to_dict(orient="list") == {"a": [1, 2], "b": [3, 4]})
chk("to_dict_split", iodf.to_dict(orient="split")["data"] == [[1, 3], [2, 4]])
chk("to_dict_index", iodf.to_dict(orient="index") == {0: {"a": 1, "b": 3}, 1: {"a": 2, "b": 4}})
chk("from_dict", pd.DataFrame.from_dict({"a": [1, 2], "b": [3, 4]})["a"].tolist() == [1, 2])
chk("from_dict_orient_index",
    pd.DataFrame.from_dict({"r1": [1, 2], "r2": [3, 4]}, orient="index").shape == (2, 2))
chk("to_records", iodf.to_records(index=False).tolist() == [(1, 3), (2, 4)])
html_tbl = "<table><tr><th>a</th><th>b</th></tr><tr><td>1</td><td>2</td></tr></table>"
rh = pd.read_html(io.StringIO(html_tbl))
chk("read_html", len(rh) == 1 and int(rh[0].iloc[0, 0]) == 1 and int(rh[0].iloc[0, 1]) == 2)
chk("to_html", "<table" in iodf.to_html())
chk("read_fwf", pd.read_fwf(io.StringIO("a    b\n1    2\n3    4\n"))["a"].tolist() == [1, 3])
chk("read_table", pd.read_table(io.StringIO("a\tb\n1\t2\n3\t4\n"))["a"].tolist() == [1, 3])
chk("read_json_index",
    pd.read_json(io.StringIO(iodf.to_json(orient="index")), orient="index")["a"].tolist() == [1, 2])
chk("read_json_columns",
    pd.read_json(io.StringIO(iodf.to_json(orient="columns")), orient="columns")["a"].tolist() == [1, 2])
chk("to_json_table", "schema" in iodf.to_json(orient="table"))
chk("to_latex", "tabular" in iodf.to_latex())
chk("to_string", "a" in iodf.to_string())
# to_markdown requires the optional 'tabulate' package; assert only when it is importable.
try:
    import tabulate  # noqa: F401
    chk("to_markdown", "|" in iodf.to_markdown())
except ImportError:
    # HONEST-SKIP: to_markdown needs the optional 'tabulate' dependency, absent from the base build.
    pass

# ---------------------------------------------------------------- Index objects & set operations
i1 = pd.Index([1, 2, 3])
i2 = pd.Index([2, 3, 4])
chk("index_union", i1.union(i2).tolist() == [1, 2, 3, 4])
chk("index_intersection", i1.intersection(i2).tolist() == [2, 3])
chk("index_difference", i1.difference(i2).tolist() == [1])
chk("index_symmetric_difference", i1.symmetric_difference(i2).tolist() == [1, 4])
chk("range_index", pd.RangeIndex(5).tolist() == [0, 1, 2, 3, 4])
chk("multiindex_from_product", len(pd.MultiIndex.from_product([["a", "b"], [1, 2]])) == 4)
chk("multiindex_from_arrays",
    pd.MultiIndex.from_arrays([["a", "a"], [1, 2]]).tolist() == [("a", 1), ("a", 2)])
chk("index_get_loc", pd.Index(["x", "y", "z"]).get_loc("y") == 1)
chk("index_get_indexer", pd.Index([1, 2, 3]).get_indexer([2, 3]).tolist() == [1, 2])
ivi = pd.IntervalIndex.from_breaks([0, 1, 2])
chk("interval_index", ivi.contains(0.5).tolist() == [True, False])
chk("interval", pd.Interval(0, 1).length == 1)
chk("index_isin", pd.Index([1, 2, 3]).isin([2, 3]).tolist() == [False, True, True])
chk("index_duplicated", pd.Index([1, 1, 2]).duplicated().tolist() == [False, True, False])
chk("index_sort_values", pd.Index([3, 1, 2]).sort_values().tolist() == [1, 2, 3])
chk("index_unique", pd.Index([1, 1, 2]).unique().tolist() == [1, 2])
chk("index_to_frame", pd.Index([1, 2], name="k").to_frame().columns.tolist() == ["k"])
chk("categorical_index", pd.CategoricalIndex(["a", "b", "a"]).codes.tolist() == [0, 1, 0])

# ---------------------------------------------------------------- combining / arithmetic alignment
sa = pd.Series([1, 2, 3], index=["a", "b", "c"])
sb = pd.Series([10, 20], index=["b", "c"])
chk("add_fill_value", sa.add(sb, fill_value=0).sort_index().tolist() == [1.0, 12.0, 23.0])
chk("sub_fill_value", sa.sub(sb, fill_value=0).sort_index().tolist() == [1.0, -8.0, -17.0])
chk("mul_fill_value", sa.mul(sb, fill_value=1).sort_index().tolist() == [1.0, 20.0, 60.0])
chk("div_fill_value", abs(sa.div(sb, fill_value=1).sort_index().iloc[1] - 0.2) < 1e-9)
adf = pd.DataFrame({"a": [1, 2], "b": [3, 4]})
chk("df_add_axis",
    adf.add(pd.Series([10, 20], index=["a", "b"]), axis=1).values.tolist() == [[11, 23], [12, 24]])
chk("df_sub_axis",
    adf.sub(pd.Series([1, 1], index=["a", "b"]), axis=1).values.tolist() == [[0, 2], [1, 3]])
chk("rename_columns", pd.DataFrame({"a": [1]}).rename(columns={"a": "A"}).columns.tolist() == ["A"])
chk("rename_index",
    pd.DataFrame({"a": [1]}, index=[0]).rename(index={0: "r0"}).index.tolist() == ["r0"])
chk("drop_columns", pd.DataFrame({"a": [1], "b": [2]}).drop(columns=["b"]).columns.tolist() == ["a"])
chk("drop_index", pd.DataFrame({"a": [1, 2]}, index=[0, 1]).drop(index=0).index.tolist() == [1])
chk("agg_list",
    pd.DataFrame({"a": [1, 2, 3], "b": [4, 5, 6]}).agg(["sum", "mean"]).loc["sum"].tolist() == [6, 15])
chk("aggregate_dict",
    pd.DataFrame({"a": [1, 2, 3], "b": [4, 5, 6]}).aggregate({"a": "sum", "b": "max"}).to_dict()
    == {"a": 6, "b": 6})
udf = pd.DataFrame({"a": [1, 2, 3]})
udf.update(pd.DataFrame({"a": [10, np.nan, 30]}))
chk("update", udf["a"].tolist() == [10, 2, 30])
chk("combine", pd.Series([1, 2]).combine(pd.Series([3, 1]), max).tolist() == [3, 2])
chk("combine_first_series",
    pd.Series([1.0, np.nan]).combine_first(pd.Series([9.0, 9.0])).tolist() == [1.0, 9.0])
al, ar = pd.Series([1, 2], index=["a", "b"]).align(pd.Series([3, 4], index=["b", "c"]))
chk("align", al.index.tolist() == ["a", "b", "c"])
chk("series_diff", pd.Series([1, 3, 6]).diff().iloc[1:].tolist() == [2.0, 3.0])
chk("df_diff", pd.DataFrame({"a": [1, 3, 6]}).diff()["a"].iloc[1:].tolist() == [2.0, 3.0])

# ---------------------------------------------------------------- describe(include='all') for objects
descall = pd.DataFrame({"n": [1, 2, 3], "s": ["a", "a", "b"]}).describe(include="all")
chk("describe_all_top", descall.loc["top", "s"] == "a")
chk("describe_all_unique", int(descall.loc["unique", "s"]) == 2)
chk("describe_all_count", abs(descall.loc["count", "n"] - 3.0) < 1e-9)

# ---------------------------------------------------------------- HONEST-SKIP (documented, not tested)
# Plotting (DataFrame.plot / Series.plot): requires matplotlib + a display backend; no display on
#   the StarryOS target, so plotting is intentionally not exercised here.
# Styling (DataFrame.style / Styler): HTML/CSS rendering for notebooks; no display, out of scope.
# Global options / display (pd.set_option, pd.options, pd.describe_option): mutate process-global
#   display state and affect only repr/formatting, not data semantics; skipped by policy.
# to_markdown asserted only when the optional 'tabulate' package is importable (see above).

print("PANDAS_RESULT ok=%d fail=%d" % (ok, fail))
if fail == 0:
    print("PANDAS_DONE")
    sys.exit(0)
sys.exit(1)
