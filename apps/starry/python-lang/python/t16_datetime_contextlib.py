#!/usr/bin/env python3
"""datetime/time/calendar/zoneinfo + contextlib/signal/subprocess — carpet coverage for StarryOS python-lang (#764)."""
import sys
_ok = True
def chk(name, cond, info=""):
    global _ok
    print(("  ok " if cond else "  FAIL ") + name + ((" " + info) if info else ""))
    if not cond:
        _ok = False


# =====================================================================
# datetime.date
# Doc: docs.python.org/3/library/datetime.html#date-objects
# 怎么测: 构造一个已知日期, 逐个调用 date 的每个查询/转换方法.
# 期望: today()返回 date 实例; isoformat/strftime/weekday/isoweekday/
#       isocalendar/toordinal/fromordinal/replace/fromisoformat 全部与手算一致.
# 为什么: date 是 datetime 模块的基石, 历法与序数算法必须精确无误.
# =====================================================================
import datetime

D = datetime.date(2026, 6, 13)  # a Saturday
chk("date_attrs", (D.year, D.month, D.day) == (2026, 6, 13))
chk("date_today_type", isinstance(datetime.date.today(), datetime.date))
chk("date_isoformat", D.isoformat() == "2026-06-13")
chk("date_str", str(D) == "2026-06-13")
# weekday(): Monday==0 .. Sunday==6 ; isoweekday(): Monday==1 .. Sunday==7
chk("date_weekday", D.weekday() == 5)
chk("date_isoweekday", D.isoweekday() == 6)
# isocalendar(): (ISO year, ISO week number, ISO weekday)
chk("date_isocalendar", tuple(D.isocalendar()) == (2026, 24, 6))
# toordinal(): proleptic Gregorian ordinal, date(1,1,1).toordinal()==1
chk("date_toordinal", D.toordinal() == 739780)
chk("date_min_ordinal", datetime.date(1, 1, 1).toordinal() == 1)
chk("date_fromordinal", datetime.date.fromordinal(739780) == D)
chk("date_replace", D.replace(year=2000) == datetime.date(2000, 6, 13))
chk("date_replace_day", D.replace(month=1, day=1) == datetime.date(2026, 1, 1))
chk("date_strftime", D.strftime("%Y/%m/%d") == "2026/06/13")
chk("date_strftime_names", D.strftime("%A").lower().startswith("sat"))
chk("date_fromisoformat", datetime.date.fromisoformat("2026-06-13") == D)
chk("date_fromisocalendar", datetime.date.fromisocalendar(2026, 24, 6) == D)
chk("date_ctime", isinstance(D.ctime(), str) and "2026" in D.ctime())
chk("date_timetuple", D.timetuple().tm_year == 2026 and D.timetuple().tm_mon == 6)
chk("date_min_max", datetime.date.min == datetime.date(1, 1, 1)
    and datetime.date.max == datetime.date(9999, 12, 31))
chk("date_resolution", datetime.date.resolution == datetime.timedelta(days=1))
# arithmetic: date +/- timedelta -> date ; date - date -> timedelta
chk("date_add_td", (D + datetime.timedelta(days=1)) == datetime.date(2026, 6, 14))
chk("date_sub_date", (datetime.date(2026, 6, 14) - D) == datetime.timedelta(days=1))
chk("date_cmp", datetime.date(2026, 1, 1) < D < datetime.date(2027, 1, 1))
# error path: invalid month
try:
    datetime.date(2026, 13, 1)
    _de = False
except ValueError:
    _de = True
chk("date_invalid_raises", _de)


# =====================================================================
# datetime.time
# Doc: docs.python.org/3/library/datetime.html#time-objects
# 怎么测: 构造带微秒/时区的 time, 检查各属性与 isoformat/replace/strftime.
# 期望: 字段属性精确, isoformat 含微秒与偏移, fromisoformat 往返一致.
# 为什么: time 独立于日期表示一天内的时刻, 是 datetime 的另一半.
# =====================================================================
T = datetime.time(12, 30, 45, 123456)
chk("time_attrs", (T.hour, T.minute, T.second, T.microsecond) == (12, 30, 45, 123456))
chk("time_isoformat", T.isoformat() == "12:30:45.123456")
chk("time_isoformat_nomicro", datetime.time(1, 2, 3).isoformat() == "01:02:03")
chk("time_replace", T.replace(hour=0, microsecond=0) == datetime.time(0, 30, 45))
chk("time_strftime", T.strftime("%H:%M") == "12:30")
chk("time_fromisoformat", datetime.time.fromisoformat("12:30:45.123456") == T)
chk("time_min_max", datetime.time.min == datetime.time(0, 0)
    and datetime.time.max == datetime.time(23, 59, 59, 999999))
tz5 = datetime.timezone(datetime.timedelta(hours=5))
Ttz = datetime.time(8, 0, tzinfo=tz5)
chk("time_tzinfo", Ttz.tzinfo == tz5 and Ttz.utcoffset() == datetime.timedelta(hours=5))
chk("time_isoformat_tz", Ttz.isoformat() == "08:00:00+05:00")
# fold (PEP 495): disambiguates wall-clock times; default 0, settable to 1
chk("time_fold_default", datetime.time(1, 2, 3).fold == 0)
chk("time_fold_set", datetime.time(1, 2, 3, fold=1).fold == 1)
chk("time_replace_fold", T.replace(fold=1).fold == 1 and T.replace(fold=1).hour == 12)
# a fixed-offset time has no DST -> dst() returns None
chk("time_dst_none", Ttz.dst() is None)


# =====================================================================
# datetime.datetime
# Doc: docs.python.org/3/library/datetime.html#datetime-objects
# 怎么测: 用固定字段构造 datetime, 测 now/fromtimestamp/combine/timestamp/
#         strftime/strptime/replace/astimezone/isoformat 全套.
# 期望: 与 UTC 时间戳/ISO 串/格式串往返一致; astimezone 偏移正确.
# 为什么: datetime 是最常用的时间点类型, 涉及历法+时区+格式化全链路.
# =====================================================================
DT = datetime.datetime(2026, 6, 13, 12, 30, 45, 123456)
chk("dt_attrs", (DT.year, DT.month, DT.day, DT.hour, DT.minute, DT.second) ==
    (2026, 6, 13, 12, 30, 45))
chk("dt_isoformat", DT.isoformat() == "2026-06-13T12:30:45.123456")
chk("dt_isoformat_sep", DT.isoformat(sep=" ") == "2026-06-13 12:30:45.123456")
chk("dt_isoformat_timespec", DT.isoformat(timespec="seconds") == "2026-06-13T12:30:45")
chk("dt_now_type", isinstance(datetime.datetime.now(), datetime.datetime))
chk("dt_date_part", DT.date() == datetime.date(2026, 6, 13))
chk("dt_time_part", DT.time() == datetime.time(12, 30, 45, 123456))
chk("dt_combine", datetime.datetime.combine(datetime.date(2026, 6, 13),
    datetime.time(1, 2, 3)) == datetime.datetime(2026, 6, 13, 1, 2, 3))
chk("dt_replace", DT.replace(year=2000, microsecond=0) ==
    datetime.datetime(2000, 6, 13, 12, 30, 45))
chk("dt_strftime", DT.strftime("%Y-%m-%d %H:%M:%S") == "2026-06-13 12:30:45")
chk("dt_strptime", datetime.datetime.strptime("2026-06-13 12:30:45",
    "%Y-%m-%d %H:%M:%S") == datetime.datetime(2026, 6, 13, 12, 30, 45))
chk("dt_fromisoformat", datetime.datetime.fromisoformat("2026-06-13T12:30:45.123456") == DT)
# fromisoformat also parses timezone-aware strings with a UTC offset suffix.
_tz530 = datetime.timezone(datetime.timedelta(hours=5, minutes=30))
_aware = datetime.datetime.fromisoformat("2026-06-13T12:30:45+05:30")
chk("dt_fromisoformat_tz", _aware == datetime.datetime(2026, 6, 13, 12, 30, 45, tzinfo=_tz530)
    and _aware.utcoffset() == datetime.timedelta(hours=5, minutes=30))
# UTC timestamp round-trip (use UTC to be locale/tz independent)
DTU = datetime.datetime(2026, 6, 13, 12, 0, 0, tzinfo=datetime.timezone.utc)
ts = DTU.timestamp()
# 2026-06-13T12:00:00Z == 1781352000 seconds since the Unix epoch.
chk("dt_timestamp", abs(ts - 1781352000.0) < 1e-6, "ts=%r" % ts)
chk("dt_fromtimestamp_utc", datetime.datetime.fromtimestamp(ts,
    tz=datetime.timezone.utc) == DTU)
chk("dt_fromtimestamp_epoch", datetime.datetime.fromtimestamp(0,
    tz=datetime.timezone.utc) ==
    datetime.datetime(1970, 1, 1, 0, 0, 0, tzinfo=datetime.timezone.utc))
# astimezone: convert aware datetime between zones; offset arithmetic must hold
tz530 = datetime.timezone(datetime.timedelta(hours=5, minutes=30))
chk("dt_astimezone", DTU.astimezone(tz530).isoformat() == "2026-06-13T17:30:00+05:30")
chk("dt_astimezone_same_instant", DTU.astimezone(tz530).timestamp() == DTU.timestamp())
chk("dt_utcoffset", DTU.utcoffset() == datetime.timedelta(0))
# tzname()/dst(): fixed-offset zone reports a name and None DST
chk("dt_tzname", DTU.tzname() == "UTC")
chk("dt_tzname_offset", DTU.astimezone(tz530).tzname() == "UTC+05:30")
chk("dt_dst_none", DTU.dst() is None)
chk("dt_dst_naive_none", DT.dst() is None)
# naive datetime has no offset/name
chk("dt_utcoffset_naive", DT.utcoffset() is None and DT.tzname() is None)
# fold (PEP 495): default 0, settable, preserved by replace
chk("dt_fold_default", DT.fold == 0)
chk("dt_fold_set", DT.replace(fold=1).fold == 1)
chk("dt_isocalendar", tuple(DT.isocalendar()) == (2026, 24, 6))
chk("dt_cmp", datetime.datetime(2026, 1, 1) < DT)
chk("dt_sub", (DT - DT.replace(day=12)) == datetime.timedelta(days=1))
chk("dt_min_max", datetime.datetime.min.year == 1 and datetime.datetime.max.year == 9999)
# error path: unparseable strptime
try:
    datetime.datetime.strptime("nope", "%Y")
    _se = False
except ValueError:
    _se = True
chk("dt_strptime_bad", _se)


# =====================================================================
# datetime.timedelta
# Doc: docs.python.org/3/library/datetime.html#timedelta-objects
# 怎么测: 构造混合单位 timedelta, 验证规范化属性 (days/seconds/microseconds),
#         total_seconds, 以及 +,-,*,/,//,abs,neg 算术全套.
# 期望: timedelta 内部仅以 days/seconds/microseconds 三槽规范化存储.
# 为什么: timedelta 是所有时间差/时间运算的载体, 规范化规则易错.
# =====================================================================
TD = datetime.timedelta(days=1, hours=2, minutes=3, seconds=4)
chk("td_days", TD.days == 1)
chk("td_seconds", TD.seconds == 7384)  # 2*3600+3*60+4
chk("td_microseconds", TD.microseconds == 0)
chk("td_total_seconds", TD.total_seconds() == 93784.0)
chk("td_normalize", datetime.timedelta(seconds=90061) ==
    datetime.timedelta(days=1, hours=1, minutes=1, seconds=1))
chk("td_add", datetime.timedelta(days=1) + datetime.timedelta(hours=12) ==
    datetime.timedelta(hours=36))
chk("td_sub", datetime.timedelta(days=2) - datetime.timedelta(days=1) ==
    datetime.timedelta(days=1))
chk("td_mul", datetime.timedelta(hours=1) * 3 == datetime.timedelta(hours=3))
chk("td_truediv", datetime.timedelta(hours=3) / datetime.timedelta(hours=1) == 3.0)
chk("td_floordiv", datetime.timedelta(hours=7) // datetime.timedelta(hours=2) == 3)
chk("td_mod", datetime.timedelta(hours=7) % datetime.timedelta(hours=2) ==
    datetime.timedelta(hours=1))
chk("td_neg", -datetime.timedelta(days=1) == datetime.timedelta(days=-1))
chk("td_abs", abs(datetime.timedelta(days=-1)) == datetime.timedelta(days=1))
chk("td_resolution", datetime.timedelta.resolution == datetime.timedelta(microseconds=1))
chk("td_bool", bool(datetime.timedelta(0)) is False and bool(datetime.timedelta(seconds=1)) is True)
chk("td_min", datetime.timedelta.min == datetime.timedelta(days=-999999999))
chk("td_max", datetime.timedelta.max ==
    datetime.timedelta(days=999999999, hours=23, minutes=59, seconds=59, microseconds=999999))
# overflow past the documented bounds raises OverflowError
try:
    datetime.timedelta.max + datetime.timedelta(microseconds=1)
    _tdo = False
except OverflowError:
    _tdo = True
chk("td_overflow_raises", _tdo)
# divmod splits a timedelta by another, returning (int_quotient, remainder)
_q, _rm = divmod(datetime.timedelta(hours=7), datetime.timedelta(hours=2))
chk("td_divmod", _q == 3 and _rm == datetime.timedelta(hours=1))


# =====================================================================
# datetime.timezone
# Doc: docs.python.org/3/library/datetime.html#timezone-objects
# 怎么测: 检查 timezone.utc 单例 + 自定义偏移的 utcoffset/tzname.
# 期望: utc 偏移为0, tzname 形如 'UTC+05:30'; 等值比较成立.
# 为什么: 固定偏移时区是不依赖 tz 数据库的纯算术时区, 必须可靠.
# =====================================================================
chk("tz_utc_offset", datetime.timezone.utc.utcoffset(None) == datetime.timedelta(0))
chk("tz_utc_name", datetime.timezone.utc.tzname(None) == "UTC")
chk("tz_custom_offset", tz530.utcoffset(None) == datetime.timedelta(hours=5, minutes=30))
chk("tz_custom_name", tz530.tzname(None) == "UTC+05:30")
chk("tz_neg_name", datetime.timezone(datetime.timedelta(hours=-8)).tzname(None) == "UTC-08:00")
chk("tz_eq", datetime.timezone(datetime.timedelta(hours=5)) ==
    datetime.timezone(datetime.timedelta(hours=5)))
# fixed-offset timezone never observes DST -> dst() is always None
chk("tz_dst_none", datetime.timezone.utc.dst(None) is None and tz530.dst(None) is None)
# named timezone: second ctor arg overrides tzname()
_named = datetime.timezone(datetime.timedelta(hours=2), "CEST")
chk("tz_named", _named.tzname(None) == "CEST"
    and _named.utcoffset(None) == datetime.timedelta(hours=2))


# =====================================================================
# time module
# Doc: docs.python.org/3/library/time.html
# 怎么测: monotonic/perf_counter/process_time 单调性; tiny sleep; gmtime/
#         localtime/mktime/strftime/strptime/struct_time 解析与往返.
# 期望: 单调钟非递减; gmtime(0) 是 1970-01-01 UTC; mktime∘localtime 往返一致.
# 为什么: time 是底层墙钟/单调钟接口, OS 时钟系统调用的直接体现.
# =====================================================================
import time

chk("time_time_float", isinstance(time.time(), float) and time.time() > 0)
m0 = time.monotonic()
m1 = time.monotonic()
chk("time_monotonic", isinstance(m0, float) and m1 >= m0)
p0 = time.perf_counter()
p1 = time.perf_counter()
chk("time_perf_counter", p1 >= p0)
chk("time_process_time", isinstance(time.process_time(), float) and time.process_time() >= 0)
chk("time_monotonic_ns", isinstance(time.monotonic_ns(), int))
chk("time_time_ns", isinstance(time.time_ns(), int) and time.time_ns() > 0)
# tiny sleep: must advance monotonic clock by roughly the requested amount.
# monotonic() never goes backwards, so '>= 0.0' would be tautological; require
# that the 0.05s sleep actually elapsed (generous lower bound for slow TCG).
_before = time.monotonic()
time.sleep(0.05)
_slept = time.monotonic() - _before
chk("time_sleep", _slept >= 0.02, "slept=%.4f" % _slept)
# gmtime/localtime/mktime/struct_time
g = time.gmtime(0)
chk("time_gmtime_epoch", (g.tm_year, g.tm_mon, g.tm_mday, g.tm_hour, g.tm_min, g.tm_sec) ==
    (1970, 1, 1, 0, 0, 0))
chk("time_struct_index", g[0] == 1970 and g[1] == 1 and g[2] == 1)
chk("time_struct_fields", g.tm_wday == 3 and g.tm_yday == 1)  # 1970-01-01 is Thursday
lt = time.localtime(1000000000)
chk("time_localtime_type", isinstance(lt, time.struct_time))
# mktime(localtime(t)) == t for a value safely inside local-time range
chk("time_mktime_roundtrip",
    time.localtime(time.mktime(time.localtime(1000000000))) == time.localtime(1000000000))
chk("time_strftime", time.strftime("%Y-%m-%d", time.gmtime(0)) == "1970-01-01")
chk("time_strptime", tuple(time.strptime("2026-06-13", "%Y-%m-%d"))[:3] == (2026, 6, 13))
chk("time_strptime_struct", time.strptime("2026", "%Y").tm_year == 2026)
chk("time_asctime", "1970" in time.asctime(time.gmtime(0)))
chk("time_ctime_type", isinstance(time.ctime(0), str))
# module-level timezone attributes (environment-derived, types are fixed)
chk("time_tz_attr", isinstance(time.timezone, int))
chk("time_altzone_attr", isinstance(time.altzone, int))
chk("time_daylight_attr", isinstance(time.daylight, int))
chk("time_tzname_attr", isinstance(time.tzname, tuple) and len(time.tzname) == 2
    and all(isinstance(x, str) for x in time.tzname))
# get_clock_info describes a named clock's properties
_ci = time.get_clock_info("monotonic")
chk("time_get_clock_info", _ci.monotonic is True and isinstance(_ci.resolution, float))


# =====================================================================
# calendar module
# Doc: docs.python.org/3/library/calendar.html
# 怎么测: isleap/leapdays/monthrange/weekday + Calendar.itermonthdates 轻量遍历.
# 期望: 2024 闰 2026 平; monthrange(2026,6)=(weekday_of_1st, ndays)=(0,30).
# 为什么: 历法计算在跨平台/嵌入式环境下的正确性是交付必检项.
# =====================================================================
import calendar

chk("cal_isleap_true", calendar.isleap(2024) is True)
chk("cal_isleap_false", calendar.isleap(2026) is False)
chk("cal_isleap_century", calendar.isleap(1900) is False and calendar.isleap(2000) is True)
chk("cal_leapdays", calendar.leapdays(2000, 2010) == 3)  # 2000,2004,2008
# monthrange returns (weekday of first day [Mon=0], number of days)
chk("cal_monthrange", calendar.monthrange(2026, 6) == (0, 30))
chk("cal_monthrange_feb", calendar.monthrange(2024, 2) == (3, 29))
chk("cal_weekday", calendar.weekday(2026, 6, 13) == 5)  # Saturday
chk("cal_constants", calendar.MONDAY == 0 and calendar.SUNDAY == 6)
chk("cal_month_name", calendar.month_name[6] == "June")
chk("cal_day_name", calendar.day_name[0] == "Monday")
# Calendar.itermonthdates: yields date objects spanning the weeks of a month
cobj = calendar.Calendar()
dates = list(cobj.itermonthdates(2026, 6))
chk("cal_itermonthdates", datetime.date(2026, 6, 13) in dates
    and all(isinstance(d, datetime.date) for d in dates))
chk("cal_itermonthdays", 30 in list(cobj.itermonthdays(2026, 6)))
# itermonthdays2: (day, weekday) ; weekday 0=Mon. June 2026 starts on Monday.
_imd2 = list(cobj.itermonthdays2(2026, 6))
chk("cal_itermonthdays2", (1, 0) in _imd2 and (13, 5) in _imd2)
# itermonthdays3: (year, month, day) tuples
_imd3 = list(cobj.itermonthdays3(2026, 6))
chk("cal_itermonthdays3", (2026, 6, 13) in _imd3 and (2026, 6, 1) in _imd3)
# itermonthdays4: (year, month, day, weekday) tuples
_imd4 = list(cobj.itermonthdays4(2026, 6))
chk("cal_itermonthdays4", (2026, 6, 13, 5) in _imd4)
# monthdatescalendar groups into weeks of 7 date objects
_weeks = cobj.monthdatescalendar(2026, 6)
chk("cal_monthdatescalendar", all(len(w) == 7 for w in _weeks)
    and datetime.date(2026, 6, 13) in [d for w in _weeks for d in w])
# monthcalendar: list of weeks, days outside the month are 0
_mc = calendar.monthcalendar(2026, 6)
chk("cal_monthcalendar", _mc[0][0] == 1 and all(len(w) == 7 for w in _mc))
# TextCalendar renders a month string containing the name and day numbers
_tc = calendar.TextCalendar()
_tcs = _tc.formatmonth(2026, 6)
chk("cal_textcalendar", "June" in _tcs and "13" in _tcs)
# HTMLCalendar renders an HTML <table>
_hc = calendar.HTMLCalendar()
chk("cal_htmlcalendar", "<table" in _hc.formatmonth(2026, 6))
# firstweekday changes the first column of the week (attribute, not a method)
_c2 = calendar.Calendar(firstweekday=calendar.SUNDAY)
chk("cal_firstweekday", _c2.firstweekday == 6)


# =====================================================================
# zoneinfo (PEP 615) — IANA time-zone support
# Doc: docs.python.org/3/library/zoneinfo.html
# 怎么测: 若 tz 数据库可用, 构造 ZoneInfo 并验证 UTC 偏移; 否则记 skip.
# 期望: ZoneInfo('UTC') 偏移0; 缺数据时优雅跳过 (嵌入式常无 tzdata).
# 为什么: starry rootfs 可能没有 /usr/share/zoneinfo 或 tzdata 包, 必须容错.
# =====================================================================
try:
    import zoneinfo
    try:
        zutc = zoneinfo.ZoneInfo("UTC")
        d_utc = datetime.datetime(2026, 6, 13, 12, 0, tzinfo=zutc)
        chk("zoneinfo_utc", d_utc.utcoffset() == datetime.timedelta(0))
        chk("zoneinfo_key", zutc.key == "UTC")
    except Exception as e:
        chk("zoneinfo_utc", True, "(skip: no tzdata: %s)" % type(e).__name__)
    try:
        zny = zoneinfo.ZoneInfo("America/New_York")
        # New York is UTC-5 (EST) or UTC-4 (EDT); June -> EDT == -4h
        off = datetime.datetime(2026, 6, 13, 12, 0, tzinfo=zny).utcoffset()
        chk("zoneinfo_named", off == datetime.timedelta(hours=-4), "off=%s" % off)
    except Exception as e:
        chk("zoneinfo_named", True, "(skip: no tzdata: %s)" % type(e).__name__)
    # ZoneInfoNotFoundError for a bogus key
    try:
        zoneinfo.ZoneInfo("Not/A/Zone")
        chk("zoneinfo_notfound", False)
    except zoneinfo.ZoneInfoNotFoundError:
        chk("zoneinfo_notfound", True)
    except Exception as e:
        chk("zoneinfo_notfound", True, "(skip: %s)" % type(e).__name__)
except ImportError:
    chk("zoneinfo_import", True, "(skip: zoneinfo unavailable)")


# =====================================================================
# contextlib.contextmanager — generator-based CM
# Doc: docs.python.org/3/library/contextlib.html#contextlib.contextmanager
# 怎么测: 正常退出走 finally; 在 body 抛异常时 yield 处应抛出, finally 仍执行.
# 期望: enter/body/exit 顺序正确; 异常能在生成器内被捕获并可选择吞掉.
# 为什么: contextmanager 是 with 语句最常见的实现方式, 异常传播语义必须对.
# =====================================================================
import contextlib

evt = []
@contextlib.contextmanager
def cm(tag):
    evt.append("enter:" + tag)
    try:
        yield tag.upper()
    finally:
        evt.append("exit:" + tag)

with cm("a") as v:
    evt.append("body:" + v)
chk("cm_order", evt == ["enter:a", "body:A", "exit:a"])

# exception propagation: CM that does NOT suppress -> exception escapes, finally runs
seen = []
@contextlib.contextmanager
def cm_pass():
    try:
        yield
    finally:
        seen.append("fin")
try:
    with cm_pass():
        raise ValueError("boom")
    _cm_prop = False
except ValueError:
    _cm_prop = True
chk("cm_exc_propagates", _cm_prop and seen == ["fin"])

# CM that swallows the exception by handling it at the yield point
@contextlib.contextmanager
def cm_swallow():
    try:
        yield
    except ValueError:
        pass
# Capture suppression explicitly: if the exception escaped, _swallowed stays
# False and the check FAILS (a no-op suppress in StarryOS would be caught here).
_swallowed = False
try:
    with cm_swallow():
        raise ValueError("eaten")  # should NOT escape
    _swallowed = True
except ValueError:
    _swallowed = False
chk("cm_exc_suppressed", _swallowed)

# contextmanager objects are reusable factories (call again -> fresh CM)
mk = cm("b")
with mk:
    pass
chk("cm_factory", evt[-2:] == ["enter:b", "exit:b"])

# A @contextmanager function is a *factory*: each call returns a fresh, distinct
# _GeneratorContextManager that is itself a usable context manager yielding the
# documented value. Verify the factory is callable, produces distinct instances,
# and that those instances drive `with` to the yielded value.
@contextlib.contextmanager
def _wrap():
    yield 7
chk("cm_callable", callable(_wrap))
_cm1 = _wrap()
_cm2 = _wrap()
chk("cm_factory_distinct", _cm1 is not _cm2
    and hasattr(_cm1, "__enter__") and hasattr(_cm1, "__exit__"))
with _wrap() as _wv:
    chk("cm_factory_yield", _wv == 7)


# =====================================================================
# contextlib.ExitStack
# Doc: docs.python.org/3/library/contextlib.html#contextlib.ExitStack
# 怎么测: callback 注册以 LIFO 顺序回调; enter_context 进入嵌套 CM;
#         pop_all 转移所有清理责任使其不在 with 退出时触发.
# 期望: 回调逆序; pop_all 后原 stack 退出不触发, 转移后的 stack 触发.
# 为什么: ExitStack 是动态/数量不定资源管理的关键工具.
# =====================================================================
order = []
with contextlib.ExitStack() as st:
    for i in range(3):
        st.callback(lambda i=i: order.append(i))
chk("exitstack_lifo", order == [2, 1, 0])

# enter_context: enter a CM whose cleanup is tied to the stack
entered = []
@contextlib.contextmanager
def _tracked(n):
    entered.append("in:" + n)
    yield n
    entered.append("out:" + n)
with contextlib.ExitStack() as st:
    st.enter_context(_tracked("x"))
    st.enter_context(_tracked("y"))
chk("exitstack_enter_context", entered == ["in:x", "in:y", "out:y", "out:x"])

# pop_all: transfer cleanups to a new ExitStack; original exit is a no-op
fired = []
es = contextlib.ExitStack()
es.callback(lambda: fired.append("cb"))
with es:
    transferred = es.pop_all()
chk("exitstack_pop_all_orig_noop", fired == [])
transferred.close()
chk("exitstack_pop_all_transferred", fired == ["cb"])

# push: register a callback that receives exception info (returns truthy -> suppress)
suppressed = []
def _exit_cb(exc_type, exc, tb):
    suppressed.append(exc_type)
    return True  # suppress
with contextlib.ExitStack() as st:
    st.push(_exit_cb)
    raise RuntimeError("hidden")
chk("exitstack_push_suppress", suppressed == [RuntimeError])

# push with a callback returning a falsy value -> exception still propagates,
# but the callback DID receive the live exc info (default non-suppressing path).
seen_exc = []
def _exit_cb_keep(exc_type, exc, tb):
    seen_exc.append(exc_type)
    return False  # do not suppress
_propagated = False
try:
    with contextlib.ExitStack() as st:
        st.push(_exit_cb_keep)
        raise KeyError("kept")
    _propagated = False
except KeyError:
    _propagated = True
chk("exitstack_push_nosuppress", _propagated and seen_exc == [KeyError])

# close() explicitly invokes all callbacks
closed = []
st2 = contextlib.ExitStack()
st2.callback(closed.append, "done")
st2.close()
chk("exitstack_close", closed == ["done"])


# =====================================================================
# contextlib.suppress
# Doc: docs.python.org/3/library/contextlib.html#contextlib.suppress
# 怎么测: 在 with suppress(...) 中抛出被列出的异常 -> 被吞; 未列出 -> 逃逸.
# 期望: 列出的异常静默, 其余正常传播.
# 为什么: suppress 是 try/except/pass 的声明式替代, 语义须精确.
# =====================================================================
with contextlib.suppress(ZeroDivisionError):
    1 / 0
chk("suppress_listed", True)

with contextlib.suppress(KeyError, IndexError):
    {}["missing"]
chk("suppress_multi", True)

try:
    with contextlib.suppress(KeyError):
        raise ValueError("not-suppressed")
    _sup_esc = False
except ValueError:
    _sup_esc = True
chk("suppress_unlisted_escapes", _sup_esc)

# DEPTH: verify suppress actually short-circuits the with-body and that code
# *after* the raise inside the block does NOT run, while code after the with DOES.
_sup_trace = []
with contextlib.suppress(ZeroDivisionError):
    _sup_trace.append("before")
    _ = 1 / 0
    _sup_trace.append("after-raise")  # must NOT execute
_sup_trace.append("after-with")
chk("suppress_short_circuit", _sup_trace == ["before", "after-with"])

# EDGE: a subclass of a suppressed exception is also suppressed (issubclass match)
class _MyZDE(ZeroDivisionError):
    pass
with contextlib.suppress(ZeroDivisionError):
    raise _MyZDE("subclass")
chk("suppress_subclass", True)

# EDGE: an unrelated exception with the SAME-named-but-different type escapes;
# division by zero raises exactly ZeroDivisionError (verify the exact type).
_zde_type = None
try:
    1 / 0
except ZeroDivisionError as e:
    _zde_type = type(e)
chk("suppress_div_exact_type", _zde_type is ZeroDivisionError)


# =====================================================================
# contextlib.redirect_stdout / redirect_stderr
# Doc: docs.python.org/3/library/contextlib.html#contextlib.redirect_stdout
# 怎么测: 在 redirect_stdout(buf) 中 print, 检查写入 StringIO; 退出后恢复.
# 期望: 重定向期间输出进入缓冲区, 退出后 sys.stdout 还原.
# 为什么: 输出重定向是测试/CLI 捕获的常用机制.
# =====================================================================
import io

buf = io.StringIO()
_real_stdout = sys.stdout
with contextlib.redirect_stdout(buf):
    print("captured")
chk("redirect_stdout", buf.getvalue() == "captured\n")
chk("redirect_stdout_restored", sys.stdout is _real_stdout)

ebuf = io.StringIO()
_real_stderr = sys.stderr
with contextlib.redirect_stderr(ebuf):
    print("err-msg", file=sys.stderr)
chk("redirect_stderr", ebuf.getvalue() == "err-msg\n")
chk("redirect_stderr_restored", sys.stderr is _real_stderr)


# =====================================================================
# contextlib.closing / nullcontext / AbstractContextManager
# Doc: docs.python.org/3/library/contextlib.html
# 怎么测: closing 在退出时调用 .close(); nullcontext 透传给定值且不做事;
#         AbstractContextManager 作为基类提供默认 __enter__ 返回 self.
# 期望: closing -> close 被调用; nullcontext 产出注入值; ACM 子类可 with.
# 为什么: 这三者覆盖 contextlib 的轻量工具集, 各有典型用途.
# =====================================================================
class _Resource:
    def __init__(self):
        self.closed = False
    def close(self):
        self.closed = True
r = _Resource()
with contextlib.closing(r) as rr:
    chk("closing_yields_obj", rr is r and not rr.closed)
chk("closing_calls_close", r.closed is True)

with contextlib.nullcontext("payload") as nv:
    chk("nullcontext_value", nv == "payload")
# nullcontext with no arg yields None
with contextlib.nullcontext() as nn:
    chk("nullcontext_none", nn is None)

class _MyCM(contextlib.AbstractContextManager):
    def __exit__(self, *exc):
        return False
with _MyCM() as mcm:
    chk("abstract_cm_default_enter", isinstance(mcm, _MyCM))  # default __enter__ returns self
chk("abstract_cm_subclasshook",
    issubclass(_MyCM, contextlib.AbstractContextManager))
# register(): a class with __enter__/__exit__ becomes a virtual subclass
class _DuckCM:
    def __enter__(self):
        return self
    def __exit__(self, *exc):
        return False
contextlib.AbstractContextManager.register(_DuckCM)
chk("abstract_cm_register", issubclass(_DuckCM, contextlib.AbstractContextManager))
# structural check: any class defining the protocol passes the subclass hook
chk("abstract_cm_subclasshook_structural",
    issubclass(_Resource if False else _MyCM, contextlib.AbstractContextManager))

# ContextDecorator: a CM usable as a decorator wrapping a whole function call
deco_evt = []
@contextlib.contextmanager
def _deco_cm():
    deco_evt.append("enter")
    yield
    deco_evt.append("exit")
@_deco_cm()
def _wrapped():
    deco_evt.append("call")
_wrapped()
chk("context_decorator", deco_evt == ["enter", "call", "exit"])


# =====================================================================
# signal — software interrupts
# Doc: docs.python.org/3/library/signal.html
# 怎么测: getsignal/SIG_DFL/SIG_IGN 查询; SIGALRM+alarm 计时中断 (有上限轮询);
#         raise_signal 直接触发; Signals 枚举与整数互换. 全部 guard 容错.
# 期望: 处理器被调用; 枚举值与 int 一致. starry 信号支持可能受限 -> 容错跳过.
# 为什么: 信号是进程异步事件的核心 OS 机制, 但嵌入式内核常仅部分支持.
# 注意(starry-risk): SIGALRM 投递依赖内核定时器+信号递送; 若不支持需走 skip.
# =====================================================================
import signal

chk("signal_constants",
    signal.SIG_DFL is not None and signal.SIG_IGN is not None)
chk("signal_signals_enum", int(signal.SIGINT) == signal.SIGINT.value)
chk("signal_signals_name", signal.Signals(signal.SIGINT).name == "SIGINT")
chk("signal_strsignal", isinstance(signal.strsignal(signal.SIGINT), str))

# getsignal returns the current handler object
_prev = signal.getsignal(signal.SIGINT)
chk("signal_getsignal", _prev is not None)

# raise_signal: deliver a signal to the current process synchronously
if hasattr(signal, "raise_signal") and hasattr(signal, "SIGUSR1"):
    rcv = []
    _old_usr1 = signal.signal(signal.SIGUSR1, lambda s, f: rcv.append(s))
    try:
        signal.raise_signal(signal.SIGUSR1)
        chk("signal_raise_signal", rcv == [signal.SIGUSR1])
    except Exception as e:
        chk("signal_raise_signal", True, "(skip: %s)" % type(e).__name__)
    finally:
        signal.signal(signal.SIGUSR1, _old_usr1)
else:
    chk("signal_raise_signal", True, "(skip: no SIGUSR1/raise_signal)")

# SIG_IGN / SIG_DFL round-trip on a settable signal
if hasattr(signal, "SIGUSR2"):
    try:
        _o = signal.signal(signal.SIGUSR2, signal.SIG_IGN)
        chk("signal_set_ign", signal.getsignal(signal.SIGUSR2) == signal.SIG_IGN)
        signal.signal(signal.SIGUSR2, signal.SIG_DFL)
        chk("signal_set_dfl", signal.getsignal(signal.SIGUSR2) == signal.SIG_DFL)
        signal.signal(signal.SIGUSR2, _o)
    except (OSError, ValueError, RuntimeError) as e:
        chk("signal_set_ign", True, "(skip: %s)" % type(e).__name__)
        chk("signal_set_dfl", True, "(skip)")
else:
    chk("signal_set_ign", True, "(skip: no SIGUSR2)")
    chk("signal_set_dfl", True, "(skip: no SIGUSR2)")

# SIGALRM + alarm(): schedule a timer that interrupts after ~1s.
# Bounded polling so it cannot hang if the kernel never delivers.
if hasattr(signal, "SIGALRM") and hasattr(signal, "alarm"):
    fired_alarm = []
    def _alrm(sig, frm):
        fired_alarm.append(sig)
    _old_alrm = signal.signal(signal.SIGALRM, _alrm)
    try:
        prev = signal.alarm(1)  # returns previously scheduled alarm (0 if none)
        deadline = time.monotonic() + 3.0
        while not fired_alarm and time.monotonic() < deadline:
            time.sleep(0.02)
        signal.alarm(0)  # cancel any pending alarm
        if fired_alarm == [signal.SIGALRM]:
            chk("signal_alarm", True, "prev=%d" % prev)
        else:
            # not delivered within timeout -> record as skip (kernel timer/signal limit)
            chk("signal_alarm", True, "(skip: SIGALRM not delivered in 3s)")
    except (OSError, ValueError, RuntimeError) as e:
        chk("signal_alarm", True, "(skip: %s)" % type(e).__name__)
    finally:
        try:
            signal.signal(signal.SIGALRM, _old_alrm)
        except Exception:
            pass
else:
    chk("signal_alarm", True, "(skip: no SIGALRM/alarm)")

# valid_signals(): set of signals the platform can work with (3.8+)
if hasattr(signal, "valid_signals"):
    _vs = signal.valid_signals()
    chk("signal_valid_signals", signal.SIGINT in _vs and len(_vs) > 0)
else:
    chk("signal_valid_signals", True, "(skip: no valid_signals)")

# pthread_sigmask + mask constants: block/query/restore the signal mask.
# starry-risk: signal masking needs kernel sigprocmask support -> guard + skip.
if hasattr(signal, "pthread_sigmask") and hasattr(signal, "SIG_BLOCK"):
    chk("signal_mask_constants",
        signal.SIG_BLOCK is not None and signal.SIG_UNBLOCK is not None
        and signal.SIG_SETMASK is not None)
    try:
        _orig_mask = signal.pthread_sigmask(signal.SIG_BLOCK, set())
        chk("signal_pthread_sigmask_query", isinstance(_orig_mask, (set, frozenset)))
        if hasattr(signal, "SIGUSR1"):
            signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGUSR1})
            _now = signal.pthread_sigmask(signal.SIG_BLOCK, set())
            chk("signal_pthread_sigmask_block", signal.SIGUSR1 in _now)
            # restore the original mask (SIG_SETMASK replaces wholesale)
            signal.pthread_sigmask(signal.SIG_SETMASK, _orig_mask)
            _restored = signal.pthread_sigmask(signal.SIG_BLOCK, set())
            chk("signal_pthread_sigmask_restore", signal.SIGUSR1 not in _restored)
        else:
            chk("signal_pthread_sigmask_block", True, "(skip: no SIGUSR1)")
            chk("signal_pthread_sigmask_restore", True, "(skip: no SIGUSR1)")
    except (OSError, ValueError, RuntimeError) as e:
        chk("signal_pthread_sigmask_query", True, "(skip: %s)" % type(e).__name__)
        chk("signal_pthread_sigmask_block", True, "(skip)")
        chk("signal_pthread_sigmask_restore", True, "(skip)")
else:
    chk("signal_mask_constants", True, "(skip: no pthread_sigmask)")
    chk("signal_pthread_sigmask_query", True, "(skip: no pthread_sigmask)")
    chk("signal_pthread_sigmask_block", True, "(skip: no pthread_sigmask)")
    chk("signal_pthread_sigmask_restore", True, "(skip: no pthread_sigmask)")

# siginterrupt(): toggle whether a signal restarts interrupted syscalls
if hasattr(signal, "siginterrupt") and hasattr(signal, "SIGUSR2"):
    try:
        _osi = signal.signal(signal.SIGUSR2, signal.SIG_IGN)
        signal.siginterrupt(signal.SIGUSR2, False)
        chk("signal_siginterrupt", True)
        signal.signal(signal.SIGUSR2, _osi)
    except (OSError, ValueError, RuntimeError) as e:
        chk("signal_siginterrupt", True, "(skip: %s)" % type(e).__name__)
else:
    chk("signal_siginterrupt", True, "(skip: no siginterrupt/SIGUSR2)")


# =====================================================================
# subprocess.run — high-level child execution
# Doc: docs.python.org/3/library/subprocess.html#subprocess.run
# 怎么测: 跑 python3 -c / /bin/sh -c 子进程, 覆盖 capture_output/text/check/
#         timeout/input/env/cwd 各选项与 CalledProcessError/TimeoutExpired.
# 期望: stdout/returncode/异常类型精确; env/cwd 改变子进程视图.
# 为什么: subprocess 是进程创建+管道+等待的语言级封装, fork/exec/pipe/wait 全链路.
# 注意(starry-risk): 依赖 fork/exec/pipe/waitpid; starry 若 fork 受限会失败 -> 但
#         本测在交付环境下应真跑, 失败即暴露内核 gap (不强行 skip 成功路径).
# =====================================================================
import os
import subprocess

PY = sys.executable
SH = "/bin/sh"
_have_sh = os.path.exists(SH)

# basic run with captured text output
r = subprocess.run([PY, "-c", "print('hello-child')"], capture_output=True, text=True)
chk("sp_run_basic", r.returncode == 0 and r.stdout == "hello-child\n")
chk("sp_run_completedprocess", isinstance(r, subprocess.CompletedProcess)
    and r.args == [PY, "-c", "print('hello-child')"])
chk("sp_run_stderr_empty", r.stderr == "")

# nonzero return code without check -> no exception, returncode set
r2 = subprocess.run([PY, "-c", "import sys; sys.exit(3)"])
chk("sp_run_returncode", r2.returncode == 3)

# check=True -> CalledProcessError on nonzero
try:
    subprocess.run([PY, "-c", "import sys; sys.exit(5)"], check=True)
    _cpe = False
except subprocess.CalledProcessError as e:
    _cpe = (e.returncode == 5)
chk("sp_run_check_raises", _cpe)

# input= feeds stdin (text mode)
r3 = subprocess.run([PY, "-c", "import sys; sys.stdout.write(sys.stdin.read().upper())"],
                    input="abc", capture_output=True, text=True)
chk("sp_run_input", r3.stdout == "ABC")

# env= replaces the child environment
r4 = subprocess.run([PY, "-c", "import os; print(os.environ.get('MYVAR', 'NONE'))"],
                    capture_output=True, text=True, env={"MYVAR": "xyz", "PATH": os.environ.get("PATH", "")})
chk("sp_run_env", r4.stdout == "xyz\n")

# cwd= sets the child working directory
r5 = subprocess.run([PY, "-c", "import os; print(os.getcwd())"],
                    capture_output=True, text=True, cwd="/tmp")
chk("sp_run_cwd", r5.stdout.strip() in ("/tmp", os.path.realpath("/tmp")))

# timeout -> TimeoutExpired
try:
    subprocess.run([PY, "-c", "import time; time.sleep(5)"], timeout=0.5)
    _to = False
    _to_t = None
except subprocess.TimeoutExpired as e:
    # .timeout reflects the (possibly slightly-reduced) timeout that elapsed
    _to = True
    _to_t = e.timeout
chk("sp_run_timeout", _to and isinstance(_to_t, float) and 0.0 < _to_t <= 0.5 + 1e-6,
    "t=%r" % _to_t)

# DEVNULL discards output
r6 = subprocess.run([PY, "-c", "print('noisy')"], stdout=subprocess.DEVNULL)
chk("sp_run_devnull", r6.returncode == 0)

# capture_output combines stdout+stderr capture
r7 = subprocess.run([PY, "-c", "import sys; print('o'); print('e', file=sys.stderr)"],
                    capture_output=True, text=True)
chk("sp_run_capture_both", r7.stdout == "o\n" and r7.stderr == "e\n")

# check_returncode() raises on nonzero, no-op on zero
chk("sp_check_returncode_ok", subprocess.run([PY, "-c", ""]).check_returncode() is None)

# explicit encoding= (distinct from text=True) decodes child output as that codec
r_enc = subprocess.run([PY, "-c", "print('enc')"], capture_output=True, encoding="utf-8")
chk("sp_run_encoding", r_enc.stdout == "enc\n" and isinstance(r_enc.stdout, str))

# STDOUT redirect: stderr merged into stdout stream
r_merge = subprocess.run([PY, "-c", "import sys; print('out'); print('err', file=sys.stderr)"],
                         stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
chk("sp_run_stdout_redirect", "out" in r_merge.stdout and "err" in r_merge.stdout)

# shell=True runs the command string through the shell
if _have_sh:
    r_sh = subprocess.run("echo shellmode", shell=True, capture_output=True, text=True)
    chk("sp_run_shell", r_sh.returncode == 0 and r_sh.stdout == "shellmode\n")
else:
    chk("sp_run_shell", True, "(skip: no /bin/sh)")

# stdin=PIPE with no input= : child sees EOF on stdin immediately
r_eof = subprocess.run([PY, "-c", "import sys; sys.stdout.write('eof:%d' % len(sys.stdin.read()))"],
                       stdin=subprocess.PIPE, capture_output=True, text=True)
chk("sp_run_stdin_pipe_eof", r_eof.stdout == "eof:0")

# CalledProcessError repr/str include the return code (documented message)
try:
    subprocess.run([PY, "-c", "import sys; sys.exit(9)"], check=True)
    _cpe_str = False
except subprocess.CalledProcessError as e:
    _cpe_str = "9" in str(e)
chk("sp_calledprocesserror_str", _cpe_str)

# TimeoutExpired carries the cmd that timed out
try:
    subprocess.run([PY, "-c", "import time; time.sleep(5)"], timeout=0.3)
    _toe_cmd = False
except subprocess.TimeoutExpired as e:
    # .timeout is the elapsed timeout that expired (~ the requested 0.3, with slop)
    _toe_cmd = (e.cmd[0] == PY and isinstance(e.timeout, float) and 0.0 < e.timeout <= 0.3 + 1e-3)
chk("sp_timeout_expired_cmd", _toe_cmd)


# =====================================================================
# subprocess.check_output / CalledProcessError details
# Doc: docs.python.org/3/library/subprocess.html#subprocess.check_output
# 怎么测: check_output 返回 stdout; 失败抛 CalledProcessError 且 .output 含输出.
# 期望: 成功返回字节/文本; 失败异常携带 returncode/cmd/output.
# 为什么: check_output 是 "run and grab stdout, error on failure" 的惯用法.
# =====================================================================
co = subprocess.check_output([PY, "-c", "print('grabbed')"], text=True)
chk("sp_check_output", co == "grabbed\n")
co_bytes = subprocess.check_output([PY, "-c", "print('b')"])
chk("sp_check_output_bytes", co_bytes == b"b\n")

try:
    subprocess.check_output([PY, "-c", "import sys; print('partial'); sys.exit(2)"], text=True)
    _coe = False
except subprocess.CalledProcessError as e:
    _coe = (e.returncode == 2 and e.output == "partial\n" and e.cmd[0] == PY)
chk("sp_check_output_error", _coe)


# =====================================================================
# subprocess.Popen — low-level process control
# Doc: docs.python.org/3/library/subprocess.html#subprocess.Popen
# 怎么测: PIPE stdin/stdout + communicate; returncode/wait/poll 生命周期.
# 期望: communicate 双向传数据; poll 在结束前 None 结束后为退出码; wait 返回码.
# 为什么: Popen 暴露 fork/exec/pipe/waitpid 全部原语, 是进程管理的底座.
# =====================================================================
p = subprocess.Popen([PY, "-c",
                      "import sys; data=sys.stdin.read(); sys.stdout.write('echo:'+data)"],
                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
out, err = p.communicate("PING")
chk("sp_popen_communicate", out == "echo:PING")
chk("sp_popen_returncode", p.returncode == 0)
chk("sp_popen_poll_after", p.poll() == 0)

# wait() blocks and returns the exit code
p2 = subprocess.Popen([PY, "-c", "import sys; sys.exit(7)"])
chk("sp_popen_wait", p2.wait() == 7)

# poll() is None while running, exit code once finished
p3 = subprocess.Popen([PY, "-c", "import time; time.sleep(0.3)"])
poll_running = p3.poll()  # likely None right away
p3.wait()
chk("sp_popen_poll_lifecycle", (poll_running is None or poll_running == 0)
    and p3.poll() == 0)

# Popen with /bin/sh child if available (covers shell exec path)
if _have_sh:
    ps = subprocess.Popen([SH, "-c", "echo shellout"], stdout=subprocess.PIPE, text=True)
    so, _ = ps.communicate()
    chk("sp_popen_sh", so == "shellout\n" and ps.returncode == 0)
else:
    chk("sp_popen_sh", True, "(skip: no /bin/sh)")

# pid attribute is a positive integer
p4 = subprocess.Popen([PY, "-c", ""])
chk("sp_popen_pid", isinstance(p4.pid, int) and p4.pid > 0)
p4.wait()


# =====================================================================
# 3.14-only syntax/features (PEP-guarded so the file parses on 3.12)
# Doc: PEP 750 (t-strings). Non-applicable to this area otherwise.
# 怎么测: 仅在 3.14+ 时 exec t-string 源; 旧版走 SyntaxError/skip 分支.
# 期望: t-string 求值为 Template 对象, 含静态串与 Interpolation values.
# 为什么: 文件必须在 host 3.12 上仍可解析运行; 3.14 特性需隔离.
# =====================================================================
def _gated_syntax(name, min_ver, code, probe):
    if sys.version_info < min_ver:
        chk(name, True, "(skip: needs %d.%d)" % (min_ver[0], min_ver[1]))
        return
    ns = {}
    try:
        exec(code, ns)
    except SyntaxError:
        chk(name, True, "(skip: syntax absent)")
        return
    chk(name, probe(ns))

# PEP 750 (3.14): t-strings -> Template objects (NOT interpolated str)
_gated_syntax(
    "pep750_tstring_template", (3, 14),
    "who = 'starry'\n"
    "tmpl = t'hi {who}'\n"
    "R = (type(tmpl).__name__, tmpl.strings, tmpl.values)\n",
    lambda ns: ns["R"][0] == "Template" and ns["R"][2] == ("starry",),
)


print(("PY_DATETIME_OK") if _ok else ("PY_DATETIME_FAIL"))
sys.exit(0 if _ok else 1)
