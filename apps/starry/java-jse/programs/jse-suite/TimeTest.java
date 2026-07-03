import java.time.*;
import java.time.chrono.*;
import java.time.format.*;
import java.time.temporal.*;
import java.time.zone.*;
import java.util.*;

/*
 * Carpet-level coverage of java.time:
 *   LocalDate / LocalTime / LocalDateTime / OffsetDateTime / OffsetTime /
 *   ZonedDateTime / Instant / Duration / Period / ZoneOffset / ZoneId /
 *   Year / YearMonth / MonthDay / Month / DayOfWeek / Clock /
 *   DateTimeFormatter / DateTimeFormatterBuilder / ChronoUnit / ChronoField /
 *   TemporalAdjusters / TemporalQueries / WeekFields / IsoChronology.
 * Normal + boundary + exception paths. Deterministic & offline (no network,
 * no system clock except via Clock.fixed). Precise equality assertions.
 */
public class TimeTest {
    static int ok = 0, fail = 0;

    static void check(boolean c, String n) {
        if (c) ok++;
        else { fail++; System.out.println("FAIL " + n); }
    }

    static void eq(Object a, Object b, String n) {
        check(a == null ? b == null : a.equals(b), n + " (got=" + a + " want=" + b + ")");
    }

    static void eqL(long a, long b, String n) {
        check(a == b, n + " (got=" + a + " want=" + b + ")");
    }

    // expect a throwable assignable to cls
    static void expect(Class<? extends Throwable> cls, Runnable r, String n) {
        try {
            r.run();
            fail++; System.out.println("FAIL " + n + " (no exception)");
        } catch (Throwable t) {
            if (cls.isInstance(t)) ok++;
            else { fail++; System.out.println("FAIL " + n + " (got " + t.getClass().getName() + ")"); }
        }
    }

    public static void main(String[] args) {
        localDate();
        localTime();
        localDateTime();
        instant();
        duration();
        period();
        zones();
        offsetTypes();
        yearTypes();
        monthDow();
        clock();
        formatters();
        chronoUnitsFields();
        adjusters();
        queriesWeekFields();
        chronology();
        exceptions();

        System.out.println("TIME_RESULT ok=" + ok + " fail=" + fail);
        if (fail == 0) System.out.println("TIME_DONE");
    }

    // ---------------------------------------------------------------- LocalDate
    static void localDate() {
        LocalDate d = LocalDate.of(2026, 5, 21);
        check(d.getDayOfWeek() == DayOfWeek.THURSDAY, "ld-dow");
        check(d.getMonth() == Month.MAY, "ld-month-enum");
        eqL(d.getMonthValue(), 5, "ld-monthvalue");
        eqL(d.getYear(), 2026, "ld-year");
        eqL(d.getDayOfMonth(), 21, "ld-dom");
        eqL(d.getDayOfYear(), 141, "ld-doy");          // 31+28+31+30+21
        eqL(d.lengthOfMonth(), 31, "ld-len-month");
        eqL(d.lengthOfYear(), 365, "ld-len-year");
        check(!d.isLeapYear(), "ld-not-leap");

        eq(d.plusDays(11), LocalDate.of(2026, 6, 1), "ld-plusdays-rollover");
        eq(d.minusDays(21), LocalDate.of(2026, 4, 30), "ld-minusdays");
        eq(d.plusMonths(1), LocalDate.of(2026, 6, 21), "ld-plusmonths");
        eq(d.plusYears(1), LocalDate.of(2027, 5, 21), "ld-plusyears");
        eq(d.plusWeeks(2), LocalDate.of(2026, 6, 4), "ld-plusweeks");
        // month-end clamping: Jan 31 + 1 month -> Feb 28 (2026 non-leap)
        eq(LocalDate.of(2026, 1, 31).plusMonths(1), LocalDate.of(2026, 2, 28), "ld-monthend-clamp");
        eq(LocalDate.of(2024, 1, 31).plusMonths(1), LocalDate.of(2024, 2, 29), "ld-monthend-leap");

        eq(d.withDayOfMonth(1), LocalDate.of(2026, 5, 1), "ld-with-dom");
        eq(d.withMonth(12), LocalDate.of(2026, 12, 21), "ld-with-month");
        eq(d.withYear(2000), LocalDate.of(2000, 5, 21), "ld-with-year");
        eq(d.withDayOfYear(1), LocalDate.of(2026, 1, 1), "ld-with-doy");

        check(d.isAfter(LocalDate.of(2026, 5, 20)), "ld-isafter");
        check(d.isBefore(LocalDate.of(2026, 5, 22)), "ld-isbefore");
        check(d.isEqual(LocalDate.of(2026, 5, 21)), "ld-isequal");
        check(d.compareTo(LocalDate.of(2026, 5, 22)) < 0, "ld-compare");

        // leap-year rule corners
        check(LocalDate.of(2000, 1, 1).isLeapYear(), "ld-leap-2000");
        check(!LocalDate.of(1900, 1, 1).isLeapYear(), "ld-leap-1900");
        check(LocalDate.of(2024, 1, 1).isLeapYear(), "ld-leap-2024");

        // epoch-day round trip + constants
        eq(LocalDate.ofEpochDay(0), LocalDate.of(1970, 1, 1), "ld-epochday0");
        eqL(LocalDate.of(1970, 1, 1).toEpochDay(), 0, "ld-toepochday0");
        eq(LocalDate.ofEpochDay(d.toEpochDay()), d, "ld-epochday-roundtrip");
        eq(LocalDate.EPOCH, LocalDate.of(1970, 1, 1), "ld-EPOCH");
        eq(LocalDate.ofYearDay(2024, 60), LocalDate.of(2024, 2, 29), "ld-ofyearday-leap");
        eq(LocalDate.ofYearDay(2026, 141), d, "ld-ofyearday");

        eqL(LocalDate.MAX.getYear(), 999_999_999L, "ld-MAX-year");
        eqL(LocalDate.MIN.getYear(), -999_999_999L, "ld-MIN-year");

        eq(d.atStartOfDay(), LocalDateTime.of(2026, 5, 21, 0, 0), "ld-atstartofday");
        eq(d.atTime(10, 30), LocalDateTime.of(2026, 5, 21, 10, 30), "ld-attime");

        eqL(d.until(LocalDate.of(2026, 6, 1), ChronoUnit.DAYS), 11, "ld-until-days");
        eqL(d.range(ChronoField.DAY_OF_MONTH).getMaximum(), 31, "ld-range-dom");

        eq(LocalDate.parse("2026-05-21"), d, "ld-parse-iso");
        eqL(d.datesUntil(d.plusDays(5)).count(), 5, "ld-datesuntil");

        check(d.isSupported(ChronoField.DAY_OF_MONTH), "ld-support-dom");
        check(!d.isSupported(ChronoField.HOUR_OF_DAY), "ld-no-support-hour");
        check(d.isSupported(ChronoUnit.DAYS), "ld-support-unit-days");
        check(!d.isSupported(ChronoUnit.HOURS), "ld-no-support-unit-hours");

        eq(LocalDate.of(2026, 1, 1).getDayOfWeek(), DayOfWeek.THURSDAY, "ld-jan1-dow");
    }

    // ---------------------------------------------------------------- LocalTime
    static void localTime() {
        LocalTime t = LocalTime.of(10, 30, 15, 500_000_000);
        eqL(t.getHour(), 10, "lt-hour");
        eqL(t.getMinute(), 30, "lt-min");
        eqL(t.getSecond(), 15, "lt-sec");
        eqL(t.getNano(), 500_000_000, "lt-nano");

        eq(t.plusHours(2), LocalTime.of(12, 30, 15, 500_000_000), "lt-plushours");
        eq(LocalTime.of(23, 0).plusHours(2), LocalTime.of(1, 0), "lt-wrap");
        eq(t.minusMinutes(45), LocalTime.of(9, 45, 15, 500_000_000), "lt-minusmin");
        eq(t.plusSeconds(50), LocalTime.of(10, 31, 5, 500_000_000), "lt-plussec");
        eq(t.withHour(0).withNano(0), LocalTime.of(0, 30, 15), "lt-withhour");

        eq(LocalTime.MIDNIGHT, LocalTime.of(0, 0), "lt-midnight");
        eq(LocalTime.NOON, LocalTime.of(12, 0), "lt-noon");
        eq(LocalTime.MIN, LocalTime.of(0, 0, 0, 0), "lt-min");
        eq(LocalTime.MAX, LocalTime.of(23, 59, 59, 999_999_999), "lt-max");

        eqL(LocalTime.of(1, 0, 0).toSecondOfDay(), 3600, "lt-secofday");
        eq(LocalTime.ofSecondOfDay(3661), LocalTime.of(1, 1, 1), "lt-ofsecofday");
        eqL(LocalTime.of(0, 0, 0, 1).toNanoOfDay(), 1, "lt-nanoofday");

        check(LocalTime.of(9, 0).isBefore(LocalTime.of(10, 0)), "lt-isbefore");
        check(LocalTime.of(11, 0).isAfter(LocalTime.of(10, 0)), "lt-isafter");

        eq(LocalTime.parse("10:30:15.5"), t, "lt-parse");
        eq(t.truncatedTo(ChronoUnit.MINUTES), LocalTime.of(10, 30), "lt-truncate");
        eqL(t.get(ChronoField.HOUR_OF_DAY), 10, "lt-get-field");
    }

    // ------------------------------------------------------------ LocalDateTime
    static void localDateTime() {
        LocalDateTime dt = LocalDateTime.of(2026, 5, 21, 10, 30, 0);
        eqL(dt.getHour(), 10, "ldt-hour");
        eqL(dt.getMinute(), 30, "ldt-min");
        eq(dt.toLocalDate(), LocalDate.of(2026, 5, 21), "ldt-tolocaldate");
        eq(dt.toLocalTime(), LocalTime.of(10, 30), "ldt-tolocaltime");

        eq(dt.plusHours(2), LocalDateTime.of(2026, 5, 21, 12, 30), "ldt-plushours");
        eq(dt.plusDays(1), LocalDateTime.of(2026, 5, 22, 10, 30), "ldt-plusdays");
        eq(LocalDateTime.of(2026, 5, 21, 23, 30).plusHours(1),
                LocalDateTime.of(2026, 5, 22, 0, 30), "ldt-day-rollover");
        eq(dt.plusMinutes(45), LocalDateTime.of(2026, 5, 21, 11, 15), "ldt-plusmin");

        eq(dt.format(DateTimeFormatter.ofPattern("yyyy-MM-dd HH:mm")), "2026-05-21 10:30", "ldt-format");
        eq(LocalDateTime.parse("2026-05-21T10:30:00"), dt, "ldt-parse-iso");
        eq(LocalDateTime.parse("2026-05-21T10:30"), dt, "ldt-parse-iso-noseconds");

        eq(dt.with(LocalTime.of(0, 0)), LocalDateTime.of(2026, 5, 21, 0, 0), "ldt-with-time");
        eq(dt.withYear(2000), LocalDateTime.of(2000, 5, 21, 10, 30), "ldt-withyear");

        check(dt.isBefore(dt.plusSeconds(1)), "ldt-isbefore");
        check(dt.isAfter(dt.minusSeconds(1)), "ldt-isafter");
        eq(dt.atZone(ZoneOffset.UTC).toLocalDateTime(), dt, "ldt-atzone-roundtrip");
        eq(dt.atOffset(ZoneOffset.ofHours(8)).getOffset(), ZoneOffset.ofHours(8), "ldt-atoffset");
        eqL(ChronoUnit.HOURS.between(dt, dt.plusHours(5)), 5, "ldt-chrono-hours");
    }

    // ----------------------------------------------------------------- Instant
    static void instant() {
        Instant i1 = Instant.ofEpochSecond(1000);
        Instant i2 = i1.plusSeconds(60);
        eqL(Duration.between(i1, i2).getSeconds(), 60, "in-between");
        eqL(i1.getEpochSecond(), 1000, "in-epochsec");
        eqL(Instant.EPOCH.getEpochSecond(), 0, "in-EPOCH");

        Instant m = Instant.ofEpochMilli(1500);
        eqL(m.getEpochSecond(), 1, "in-milli-sec");
        eqL(m.getNano(), 500_000_000, "in-milli-nano");
        eqL(m.toEpochMilli(), 1500, "in-toepochmilli");

        // nano normalization: 1.5s expressed via nanos
        Instant n = Instant.ofEpochSecond(0, 1_500_000_000L);
        eqL(n.getEpochSecond(), 1, "in-nano-norm-sec");
        eqL(n.getNano(), 500_000_000, "in-nano-norm-nano");

        check(i1.isBefore(i2), "in-isbefore");
        check(i2.isAfter(i1), "in-isafter");
        check(i1.compareTo(i2) < 0, "in-compare");

        eq(i1.plus(Duration.ofMinutes(2)), Instant.ofEpochSecond(1120), "in-plus-duration");
        eq(i2.minus(Duration.ofSeconds(60)), i1, "in-minus-duration");
        eqL(i1.until(i2, ChronoUnit.SECONDS), 60, "in-until");

        Instant frac = Instant.ofEpochSecond(1000, 750_000_000L);
        eq(frac.truncatedTo(ChronoUnit.SECONDS), i1, "in-truncate");
        eq(Instant.parse("1970-01-01T00:16:40Z"), i1, "in-parse");
        eq(i1.atZone(ZoneOffset.UTC).toInstant(), i1, "in-atzone-roundtrip");
        eq(i1.atOffset(ZoneOffset.UTC).toInstant(), i1, "in-atoffset-roundtrip");
        eq(i1.plusMillis(500).plusNanos(0), Instant.ofEpochSecond(1000, 500_000_000L), "in-plusmillis");
    }

    // ---------------------------------------------------------------- Duration
    static void duration() {
        Duration dur = Duration.ofHours(2).plusMinutes(30);
        eqL(dur.toMinutes(), 150, "dur-tomin");
        eqL(dur.toHours(), 2, "dur-tohours");
        eqL(Duration.ofDays(1).toHours(), 24, "dur-days-tohours");
        eqL(Duration.ofMinutes(1).getSeconds(), 60, "dur-min-getsec");
        eqL(Duration.ofSeconds(90).getSeconds(), 90, "dur-getsec");

        Duration ms = Duration.ofMillis(1500);
        eqL(ms.toMillis(), 1500, "dur-tomillis");
        eqL(ms.getSeconds(), 1, "dur-millis-sec");
        eqL(ms.getNano(), 500_000_000, "dur-millis-nano");
        eqL(Duration.ofNanos(1_000_000_000L).getSeconds(), 1, "dur-nanos-sec");
        eqL(Duration.ofNanos(2_500_000_000L).toNanos(), 2_500_000_000L, "dur-tonanos");

        // part accessors (Java 9+)
        Duration d3 = Duration.ofSeconds(3661);          // 1h 1m 1s
        eqL(d3.toHoursPart(), 1, "dur-hourspart");
        eqL(d3.toMinutesPart(), 1, "dur-minpart");
        eqL(d3.toSecondsPart(), 1, "dur-secpart");
        eqL(Duration.ofMillis(1500).toMillisPart(), 500, "dur-millispart");

        eq(Duration.ofHours(1).plus(Duration.ofMinutes(30)), Duration.ofMinutes(90), "dur-plus");
        eq(Duration.ofMinutes(90).minus(Duration.ofMinutes(30)), Duration.ofHours(1), "dur-minus");
        eqL(Duration.ofMinutes(10).multipliedBy(3).toMinutes(), 30, "dur-mul");
        eqL(Duration.ofMinutes(10).dividedBy(2).toMinutes(), 5, "dur-div");
        eqL(Duration.ofHours(3).dividedBy(Duration.ofHours(1)), 3, "dur-div-by-dur");
        eq(Duration.ofSeconds(5).negated(), Duration.ofSeconds(-5), "dur-negate");
        eq(Duration.ofSeconds(-5).abs(), Duration.ofSeconds(5), "dur-abs");
        check(Duration.ofSeconds(-1).isNegative(), "dur-isneg");
        check(Duration.ZERO.isZero(), "dur-iszero");
        check(Duration.ofSeconds(5).compareTo(Duration.ofSeconds(6)) < 0, "dur-compare");

        eqL(Duration.parse("PT1H30M").toMinutes(), 90, "dur-parse");
        eqL(Duration.parse("PT-6H+3M").toMinutes(), -357, "dur-parse-signed");
        eq(Duration.ofSeconds(90).toString(), "PT1M30S", "dur-tostring");

        eqL(Duration.between(LocalTime.of(10, 0), LocalTime.of(12, 30)).toMinutes(), 150, "dur-between-time");
        eq(Duration.ofHours(2).plusDays(1), Duration.ofHours(26), "dur-plusdays");
    }

    // ------------------------------------------------------------------ Period
    static void period() {
        Period p = Period.between(LocalDate.of(2026, 1, 1), LocalDate.of(2026, 5, 21));
        eqL(p.getMonths(), 4, "per-months");
        eqL(p.getDays(), 20, "per-days");
        eqL(p.getYears(), 0, "per-years");

        Period q = Period.of(1, 2, 3);
        eqL(q.getYears(), 1, "per-of-years");
        eqL(q.getMonths(), 2, "per-of-months");
        eqL(q.getDays(), 3, "per-of-days");
        eqL(q.toTotalMonths(), 14, "per-totalmonths");

        eqL(Period.ofWeeks(2).getDays(), 14, "per-ofweeks");
        eqL(Period.ofYears(3).getYears(), 3, "per-ofyears");
        eqL(Period.ofMonths(5).getMonths(), 5, "per-ofmonths");
        eqL(Period.ofDays(10).getDays(), 10, "per-ofdays");

        // normalization: 1y13m -> 2y1m  (days never normalized)
        Period nrm = Period.of(1, 13, 0).normalized();
        eqL(nrm.getYears(), 2, "per-norm-years");
        eqL(nrm.getMonths(), 1, "per-norm-months");
        eqL(Period.ofMonths(13).normalized().getYears(), 1, "per-norm-from-months");

        eq(Period.parse("P1Y2M3D"), q, "per-parse");
        eq(Period.parse("P2W"), Period.ofDays(14), "per-parse-weeks");
        check(Period.ZERO.isZero(), "per-iszero");
        check(Period.ofMonths(-1).isNegative(), "per-isneg");

        eq(Period.of(1, 1, 1).plus(Period.of(0, 1, 1)), Period.of(1, 2, 2), "per-plus");
        eq(Period.of(1, 2, 3).minus(Period.of(0, 1, 1)), Period.of(1, 1, 2), "per-minus");
        eq(Period.of(1, 2, 3).multipliedBy(2), Period.of(2, 4, 6), "per-mul");
        eq(Period.of(1, 2, 3).negated(), Period.of(-1, -2, -3), "per-negate");

        // applied to a date: 1 month + 1 day from Jan 31 2026 -> Mar 1
        eq(LocalDate.of(2026, 1, 31).plus(Period.of(0, 1, 1)), LocalDate.of(2026, 3, 1), "per-apply");
    }

    // ----------------------------------------------- ZonedDateTime / ZoneId/DST
    static void zones() {
        LocalDateTime dt = LocalDateTime.of(2026, 5, 21, 10, 30, 0);
        ZonedDateTime z = ZonedDateTime.of(dt, ZoneOffset.UTC);
        check(z.getOffset() == ZoneOffset.UTC, "zdt-offset-utc");
        eq(z.toLocalDateTime(), dt, "zdt-tolocaldt");
        eq(z.getZone(), ZoneOffset.UTC, "zdt-getzone");

        ZoneId ny = ZoneId.of("America/New_York");
        eq(ny.getId(), "America/New_York", "zone-id");
        check(ZoneId.getAvailableZoneIds().contains("America/New_York"), "zone-available-ny");
        check(ZoneId.getAvailableZoneIds().contains("UTC"), "zone-available-utc");

        // standard time (EST = -05:00) in January
        ZonedDateTime jan = ZonedDateTime.of(2026, 1, 15, 12, 0, 0, 0, ny);
        eqL(jan.getOffset().getTotalSeconds(), -5 * 3600, "zdt-est-offset");
        // daylight time (EDT = -04:00) in July
        ZonedDateTime jul = ZonedDateTime.of(2026, 7, 15, 12, 0, 0, 0, ny);
        eqL(jul.getOffset().getTotalSeconds(), -4 * 3600, "zdt-edt-offset");

        // withZoneSameInstant: 12:00Z -> 07:00 EST
        ZonedDateTime utcNoon = ZonedDateTime.of(2026, 1, 15, 12, 0, 0, 0, ZoneOffset.UTC);
        ZonedDateTime nyNoon = utcNoon.withZoneSameInstant(ny);
        eqL(nyNoon.getHour(), 7, "zdt-samewinstant-hour");
        eq(nyNoon.toInstant(), utcNoon.toInstant(), "zdt-instant-preserved");

        // withZoneSameLocal: keeps wall clock, changes offset
        ZonedDateTime sameLocal = utcNoon.withZoneSameLocal(ny);
        eqL(sameLocal.getHour(), 12, "zdt-samelocal-hour");
        eqL(sameLocal.getOffset().getTotalSeconds(), -5 * 3600, "zdt-samelocal-offset");

        // DST spring-forward gap: 2026-03-08 02:30 does not exist -> shifts to 03:30 EDT
        ZonedDateTime gap = ZonedDateTime.of(2026, 3, 8, 2, 30, 0, 0, ny);
        eqL(gap.getHour(), 3, "zdt-gap-hour");
        eqL(gap.getOffset().getTotalSeconds(), -4 * 3600, "zdt-gap-offset");

        // DST fall-back overlap: 2026-11-01 01:30 -> earlier offset EDT by default
        ZonedDateTime overlap = ZonedDateTime.of(2026, 11, 1, 1, 30, 0, 0, ny);
        eqL(overlap.getOffset().getTotalSeconds(), -4 * 3600, "zdt-overlap-earlier");
        eqL(overlap.withLaterOffsetAtOverlap().getOffset().getTotalSeconds(), -5 * 3600, "zdt-overlap-later");

        // adding 24h across spring-forward advances wall clock by 25h of local time? No:
        // plus(Duration) works on the instant timeline.
        ZonedDateTime before = ZonedDateTime.of(2026, 3, 7, 12, 0, 0, 0, ny);
        ZonedDateTime after = before.plus(Duration.ofHours(24));
        eqL(after.getHour(), 13, "zdt-dst-duration-add"); // wall clock jumps an hour due to spring-forward

        // plusDays uses local calendar (keeps wall-clock hour)
        eqL(before.plusDays(1).getHour(), 12, "zdt-plusdays-keeps-hour");

        // fixed offset zone
        ZoneId plus8 = ZoneId.of("+08:00");
        ZonedDateTime z8 = ZonedDateTime.of(dt, plus8);
        eqL(z8.getOffset().getTotalSeconds(), 8 * 3600, "zdt-fixed-offset");
        eq(ZoneId.of("Z"), ZoneOffset.UTC, "zone-Z-is-utc");

        // ISO_ZONED_DATE_TIME round trip
        ZonedDateTime parsed = ZonedDateTime.parse("2026-05-21T10:30:00Z");
        eq(parsed.toInstant(), z.toInstant(), "zdt-parse-roundtrip");
    }

    // ------------------------------- ZoneOffset / OffsetDateTime / OffsetTime
    static void offsetTypes() {
        eqL(ZoneOffset.ofHours(8).getTotalSeconds(), 28800, "zo-ofhours");
        eqL(ZoneOffset.ofHoursMinutes(5, 30).getTotalSeconds(), 19800, "zo-ofhoursmin");
        eqL(ZoneOffset.ofTotalSeconds(-3600).getTotalSeconds(), -3600, "zo-oftotal");
        eq(ZoneOffset.of("+08:00"), ZoneOffset.ofHours(8), "zo-parse");
        check(ZoneOffset.ofHours(0) == ZoneOffset.UTC, "zo-zero-is-utc");
        eq(ZoneOffset.UTC.getId(), "Z", "zo-utc-id");
        eqL(ZoneOffset.MAX.getTotalSeconds(), 18 * 3600, "zo-max");
        eqL(ZoneOffset.MIN.getTotalSeconds(), -18 * 3600, "zo-min");

        OffsetDateTime odt = OffsetDateTime.of(2026, 5, 21, 10, 30, 0, 0, ZoneOffset.ofHours(8));
        eq(odt.getOffset(), ZoneOffset.ofHours(8), "odt-offset");
        eqL(odt.getHour(), 10, "odt-hour");
        // same instant in UTC: 10:30+08:00 == 02:30Z
        OffsetDateTime inUtc = odt.withOffsetSameInstant(ZoneOffset.UTC);
        eqL(inUtc.getHour(), 2, "odt-sameinstant-hour");
        eq(odt.toInstant(), inUtc.toInstant(), "odt-instant-eq");
        check(odt.isEqual(inUtc), "odt-isequal");
        eq(OffsetDateTime.parse("2026-05-21T10:30:00+08:00"), odt, "odt-parse");
        eq(odt.toZonedDateTime().toInstant(), odt.toInstant(), "odt-tozdt");

        OffsetTime ot = OffsetTime.of(10, 30, 0, 0, ZoneOffset.ofHours(8));
        eq(ot.getOffset(), ZoneOffset.ofHours(8), "ot-offset");
        eqL(ot.withOffsetSameInstant(ZoneOffset.UTC).getHour(), 2, "ot-sameinstant");
        eq(OffsetTime.parse("10:30:00+08:00"), ot, "ot-parse");
    }

    // ---------------------------------- Year / YearMonth / MonthDay
    static void yearTypes() {
        Year y = Year.of(2024);
        check(y.isLeap(), "year-leap");
        eqL(y.length(), 366, "year-length-leap");
        eqL(Year.of(2026).length(), 365, "year-length");
        eq(y.plusYears(1), Year.of(2025), "year-plus");
        check(Year.of(2020).isBefore(Year.of(2026)), "year-isbefore");
        eq(y.atDay(60), LocalDate.of(2024, 2, 29), "year-atday-leap");
        eq(y.atMonth(5), YearMonth.of(2024, 5), "year-atmonth");
        check(Year.isLeap(2000), "year-static-leap-2000");
        check(!Year.isLeap(1900), "year-static-leap-1900");
        eqL(Year.of(2026).getValue(), 2026, "year-value");

        YearMonth ym = YearMonth.of(2026, 2);
        eqL(ym.lengthOfMonth(), 28, "ym-len");
        eqL(YearMonth.of(2024, 2).lengthOfMonth(), 29, "ym-len-leap");
        eq(ym.atDay(15), LocalDate.of(2026, 2, 15), "ym-atday");
        eq(ym.atEndOfMonth(), LocalDate.of(2026, 2, 28), "ym-endofmonth");
        eq(ym.plusMonths(1), YearMonth.of(2026, 3), "ym-plusmonths");
        eq(ym.plusMonths(11), YearMonth.of(2027, 1), "ym-plusmonths-rollover");
        check(!ym.isLeapYear(), "ym-isleap");
        eqL(ym.getMonthValue(), 2, "ym-monthvalue");
        eq(YearMonth.parse("2026-02"), ym, "ym-parse");

        MonthDay md = MonthDay.of(2, 29);
        check(md.isValidYear(2024), "md-validyear-leap");
        check(!md.isValidYear(2026), "md-validyear-nonleap");
        eq(md.atYear(2024), LocalDate.of(2024, 2, 29), "md-atyear");
        eq(MonthDay.parse("--02-29"), md, "md-parse");
        eq(MonthDay.of(5, 21).getMonth(), Month.MAY, "md-getmonth");
    }

    // --------------------------------------------------------- Month / DayOfWeek
    static void monthDow() {
        eq(Month.of(5), Month.MAY, "month-of");
        eqL(Month.MAY.getValue(), 5, "month-value");
        eq(Month.MAY.plus(1), Month.JUNE, "month-plus");
        eq(Month.DECEMBER.plus(1), Month.JANUARY, "month-plus-wrap");
        eq(Month.JANUARY.minus(1), Month.DECEMBER, "month-minus-wrap");
        eqL(Month.FEBRUARY.length(false), 28, "month-len-nonleap");
        eqL(Month.FEBRUARY.length(true), 29, "month-len-leap");
        eqL(Month.MAY.length(false), 31, "month-len-may");
        eq(Month.MAY.firstMonthOfQuarter(), Month.APRIL, "month-firstofq");
        eq(Month.valueOf("MARCH"), Month.MARCH, "month-valueof");
        eqL(Month.values().length, 12, "month-values");
        eqL(Month.MAY.firstDayOfYear(false), 121, "month-firstdayofyear"); // 31+28+31+30+1

        eq(DayOfWeek.of(4), DayOfWeek.THURSDAY, "dow-of");
        eqL(DayOfWeek.THURSDAY.getValue(), 4, "dow-value");
        eq(DayOfWeek.THURSDAY.plus(1), DayOfWeek.FRIDAY, "dow-plus");
        eq(DayOfWeek.SUNDAY.plus(1), DayOfWeek.MONDAY, "dow-plus-wrap");
        eq(DayOfWeek.MONDAY.minus(1), DayOfWeek.SUNDAY, "dow-minus-wrap");
        eq(DayOfWeek.valueOf("FRIDAY"), DayOfWeek.FRIDAY, "dow-valueof");
        eqL(DayOfWeek.values().length, 7, "dow-values");

        // display names (CLDR-backed, Locale.US deterministic)
        eq(Month.MAY.getDisplayName(TextStyle.FULL, Locale.US), "May", "month-display");
        eq(Month.JANUARY.getDisplayName(TextStyle.SHORT, Locale.US), "Jan", "month-display-short");
        eq(DayOfWeek.THURSDAY.getDisplayName(TextStyle.FULL, Locale.US), "Thursday", "dow-display");
        eq(DayOfWeek.THURSDAY.getDisplayName(TextStyle.SHORT, Locale.US), "Thu", "dow-display-short");
    }

    // ------------------------------------------------------------------- Clock
    static void clock() {
        Instant fixed = Instant.ofEpochSecond(1000);
        Clock c = Clock.fixed(fixed, ZoneOffset.UTC);
        eq(c.instant(), fixed, "clock-fixed-instant");
        eqL(c.millis(), 1_000_000, "clock-fixed-millis");
        eq(c.getZone(), ZoneOffset.UTC, "clock-getzone");

        // 1000s after epoch = 1970-01-01T00:16:40Z
        eq(LocalDate.now(c), LocalDate.of(1970, 1, 1), "clock-localdate-now");
        eq(LocalTime.now(c), LocalTime.of(0, 16, 40), "clock-localtime-now");
        eq(Instant.now(c), fixed, "clock-instant-now");
        eq(LocalDateTime.now(c), LocalDateTime.of(1970, 1, 1, 0, 16, 40), "clock-ldt-now");
        eq(ZonedDateTime.now(c).toInstant(), fixed, "clock-zdt-now");
        eq(Year.now(c), Year.of(1970), "clock-year-now");
        eq(YearMonth.now(c), YearMonth.of(1970, 1), "clock-yearmonth-now");

        // offset clock
        Clock off = Clock.offset(c, Duration.ofHours(1));
        eqL(off.instant().getEpochSecond(), 4600, "clock-offset");
        // tick to whole minute: 1000s -> 960s
        Clock tick = Clock.tick(c, Duration.ofMinutes(1));
        eqL(tick.instant().getEpochSecond(), 960, "clock-tick-minute");
        eqL(Clock.tickSeconds(ZoneOffset.UTC).getZone().equals(ZoneOffset.UTC) ? 1 : 0, 1, "clock-tickseconds-zone");
        // withZone keeps instant
        eq(c.withZone(ZoneOffset.ofHours(8)).instant(), fixed, "clock-withzone-instant");
    }

    // --------------------------------------- DateTimeFormatter / Builder
    static void formatters() {
        LocalDate d = LocalDate.of(2026, 5, 21);
        LocalDateTime dt = LocalDateTime.of(2026, 5, 21, 10, 30, 5);

        eq(DateTimeFormatter.ISO_LOCAL_DATE.format(d), "2026-05-21", "fmt-iso-local-date");
        eq(DateTimeFormatter.BASIC_ISO_DATE.format(d), "20260521", "fmt-basic-iso");
        eq(DateTimeFormatter.ISO_LOCAL_TIME.format(LocalTime.of(10, 30, 5)), "10:30:05", "fmt-iso-time");
        eq(DateTimeFormatter.ISO_LOCAL_DATE_TIME.format(dt), "2026-05-21T10:30:05", "fmt-iso-ldt");
        eq(DateTimeFormatter.ISO_INSTANT.format(Instant.ofEpochSecond(1000)), "1970-01-01T00:16:40Z", "fmt-iso-instant");

        eq(d.format(DateTimeFormatter.ofPattern("yyyy/MM/dd")), "2026/05/21", "fmt-pattern-slash");
        eq(d.format(DateTimeFormatter.ofPattern("yyyy-DDD")), "2026-141", "fmt-pattern-doy");
        eq(d.format(DateTimeFormatter.ofPattern("EEEE", Locale.US)), "Thursday", "fmt-pattern-dow");
        eq(d.format(DateTimeFormatter.ofPattern("MMM", Locale.US)), "May", "fmt-pattern-mon-short");
        eq(d.format(DateTimeFormatter.ofPattern("MMMM", Locale.US)), "May", "fmt-pattern-mon-full");
        eq(dt.format(DateTimeFormatter.ofPattern("HH:mm:ss")), "10:30:05", "fmt-pattern-time");

        // parse via formatter
        DateTimeFormatter f1 = DateTimeFormatter.ofPattern("yyyy/MM/dd");
        eq(LocalDate.parse("2026/05/21", f1), d, "fmt-parse-pattern");

        // builder: fixed-width fields + literals
        DateTimeFormatter built = new DateTimeFormatterBuilder()
                .appendValue(ChronoField.YEAR, 4)
                .appendLiteral('/')
                .appendValue(ChronoField.MONTH_OF_YEAR, 2)
                .appendLiteral('/')
                .appendValue(ChronoField.DAY_OF_MONTH, 2)
                .toFormatter();
        eq(built.format(d), "2026/05/21", "fmt-builder-format");
        eq(LocalDate.parse("2026/05/21", built), d, "fmt-builder-parse");

        // builder optional section: time is optional
        DateTimeFormatter opt = new DateTimeFormatterBuilder()
                .appendPattern("yyyy-MM-dd")
                .optionalStart().appendLiteral(' ').appendPattern("HH:mm").optionalEnd()
                .toFormatter();
        eq(opt.format(d), "2026-05-21", "fmt-builder-optional-absent");
        eq(opt.format(dt), "2026-05-21 10:30", "fmt-builder-optional-present");

        // case-insensitive parsing
        DateTimeFormatter ci = new DateTimeFormatterBuilder()
                .parseCaseInsensitive()
                .appendPattern("yyyy-MMM-dd")
                .toFormatter(Locale.US);
        eq(LocalDate.parse("2026-MAY-21", ci), d, "fmt-builder-caseinsensitive");

        // strict resolver rejects out-of-range with explicit field set
        DateTimeFormatter strict = DateTimeFormatter.ofPattern("uuuu-MM-dd").withResolverStyle(ResolverStyle.STRICT);
        eq(LocalDate.parse("2026-05-21", strict), d, "fmt-strict-ok");

        // withZone on instant formatting
        DateTimeFormatter zoned = DateTimeFormatter.ofPattern("yyyy-MM-dd HH:mm").withZone(ZoneOffset.UTC);
        eq(zoned.format(Instant.ofEpochSecond(1000)), "1970-01-01 00:16", "fmt-withzone-instant");

        // parse into TemporalAccessor then query
        TemporalAccessor ta = DateTimeFormatter.ISO_LOCAL_DATE.parse("2026-05-21");
        eq(LocalDate.from(ta), d, "fmt-parse-temporalaccessor");

        // ISO_ZONED_DATE_TIME / ISO_OFFSET_DATE_TIME
        eq(DateTimeFormatter.ISO_OFFSET_DATE_TIME.format(
                OffsetDateTime.of(2026, 5, 21, 10, 30, 0, 0, ZoneOffset.ofHours(8))),
                "2026-05-21T10:30:00+08:00", "fmt-iso-offset-dt");
    }

    // ------------------------------------------------- ChronoUnit / ChronoField
    static void chronoUnitsFields() {
        eqL(ChronoUnit.DAYS.between(LocalDate.of(2026, 5, 1), LocalDate.of(2026, 5, 21)), 20, "cu-days-between");
        eqL(ChronoUnit.MONTHS.between(LocalDate.of(2026, 1, 1), LocalDate.of(2026, 5, 21)), 4, "cu-months-between");
        eqL(ChronoUnit.YEARS.between(LocalDate.of(2020, 5, 21), LocalDate.of(2026, 5, 21)), 6, "cu-years-between");
        eqL(ChronoUnit.WEEKS.between(LocalDate.of(2026, 5, 1), LocalDate.of(2026, 5, 22)), 3, "cu-weeks-between");
        eqL(ChronoUnit.HOURS.between(Instant.ofEpochSecond(0), Instant.ofEpochSecond(7200)), 2, "cu-hours-between");
        eqL(ChronoUnit.MINUTES.between(LocalTime.of(10, 0), LocalTime.of(12, 30)), 150, "cu-min-between");

        eqL(ChronoUnit.DAYS.getDuration().toHours(), 24, "cu-days-duration");
        eqL(ChronoUnit.HOURS.getDuration().getSeconds(), 3600, "cu-hours-duration");
        check(ChronoUnit.MINUTES.isTimeBased(), "cu-min-timebased");
        check(!ChronoUnit.MINUTES.isDateBased(), "cu-min-not-datebased");
        check(ChronoUnit.MONTHS.isDateBased(), "cu-months-datebased");
        check(!ChronoUnit.MONTHS.isTimeBased(), "cu-months-not-timebased");

        // arithmetic via unit
        eq(LocalDate.of(2026, 5, 21).plus(2, ChronoUnit.WEEKS), LocalDate.of(2026, 6, 4), "cu-plus-weeks");
        eq(LocalDateTime.of(2026, 5, 21, 10, 0).plus(90, ChronoUnit.MINUTES),
                LocalDateTime.of(2026, 5, 21, 11, 30), "cu-plus-minutes");

        LocalDate d = LocalDate.of(2026, 5, 21);
        eqL(d.get(ChronoField.DAY_OF_MONTH), 21, "cf-get-dom");
        eqL(d.get(ChronoField.MONTH_OF_YEAR), 5, "cf-get-month");
        eqL(d.get(ChronoField.DAY_OF_YEAR), 141, "cf-get-doy");
        eqL(d.get(ChronoField.DAY_OF_WEEK), 4, "cf-get-dow");
        eqL(d.getLong(ChronoField.EPOCH_DAY), d.toEpochDay(), "cf-epochday");
        eqL(ChronoField.MONTH_OF_YEAR.range().getMaximum(), 12, "cf-month-range-max");
        eqL(ChronoField.HOUR_OF_DAY.range().getMaximum(), 23, "cf-hour-range-max");
        check(ChronoField.DAY_OF_MONTH.isDateBased(), "cf-dom-datebased");
        check(ChronoField.HOUR_OF_DAY.isTimeBased(), "cf-hour-timebased");
        // with field
        eq(d.with(ChronoField.DAY_OF_MONTH, 1), LocalDate.of(2026, 5, 1), "cf-with");
        eqL(LocalTime.of(10, 30).get(ChronoField.MINUTE_OF_HOUR), 30, "cf-minute");
    }

    // ----------------------------------------------------------- TemporalAdjusters
    static void adjusters() {
        LocalDate d = LocalDate.of(2026, 5, 21);  // Thursday
        eq(d.with(TemporalAdjusters.firstDayOfMonth()), LocalDate.of(2026, 5, 1), "adj-firstdom");
        eq(d.with(TemporalAdjusters.lastDayOfMonth()), LocalDate.of(2026, 5, 31), "adj-lastdom");
        eq(d.with(TemporalAdjusters.firstDayOfNextMonth()), LocalDate.of(2026, 6, 1), "adj-firstnextmonth");
        eq(d.with(TemporalAdjusters.firstDayOfYear()), LocalDate.of(2026, 1, 1), "adj-firstdoy");
        eq(d.with(TemporalAdjusters.lastDayOfYear()), LocalDate.of(2026, 12, 31), "adj-lastdoy");
        eq(d.with(TemporalAdjusters.firstDayOfNextYear()), LocalDate.of(2027, 1, 1), "adj-firstnextyear");

        eq(d.with(TemporalAdjusters.next(DayOfWeek.MONDAY)), LocalDate.of(2026, 5, 25), "adj-next-mon");
        eq(d.with(TemporalAdjusters.previous(DayOfWeek.MONDAY)), LocalDate.of(2026, 5, 18), "adj-prev-mon");
        eq(d.with(TemporalAdjusters.nextOrSame(DayOfWeek.THURSDAY)), d, "adj-nextorsame-same");
        eq(d.with(TemporalAdjusters.previousOrSame(DayOfWeek.THURSDAY)), d, "adj-prevorsame-same");
        eq(d.with(TemporalAdjusters.nextOrSame(DayOfWeek.FRIDAY)), LocalDate.of(2026, 5, 22), "adj-nextorsame");

        // Mondays in May 2026: 4, 11, 18, 25
        eq(d.with(TemporalAdjusters.firstInMonth(DayOfWeek.MONDAY)), LocalDate.of(2026, 5, 4), "adj-firstinmonth");
        eq(d.with(TemporalAdjusters.lastInMonth(DayOfWeek.MONDAY)), LocalDate.of(2026, 5, 25), "adj-lastinmonth");
        eq(d.with(TemporalAdjusters.dayOfWeekInMonth(2, DayOfWeek.MONDAY)), LocalDate.of(2026, 5, 11), "adj-dowinmonth");

        // custom adjuster
        TemporalAdjuster plusOne = TemporalAdjusters.ofDateAdjuster(x -> x.plusDays(1));
        eq(d.with(plusOne), LocalDate.of(2026, 5, 22), "adj-custom");

        // adjuster on a LocalDateTime keeps time component
        LocalDateTime dt = LocalDateTime.of(2026, 5, 21, 10, 30);
        eq(dt.with(TemporalAdjusters.firstDayOfMonth()), LocalDateTime.of(2026, 5, 1, 10, 30), "adj-on-ldt");
    }

    // --------------------------------------- TemporalQueries / WeekFields
    static void queriesWeekFields() {
        LocalDate d = LocalDate.of(2026, 5, 21);
        check(d.query(TemporalQueries.precision()) == ChronoUnit.DAYS, "tq-precision-date");
        check(LocalTime.of(10, 0).query(TemporalQueries.precision()) == ChronoUnit.NANOS, "tq-precision-time");
        check(d.query(TemporalQueries.localDate()) == d || d.query(TemporalQueries.localDate()).equals(d), "tq-localdate");
        check(d.query(TemporalQueries.localTime()) == null, "tq-localtime-null");
        check(d.query(TemporalQueries.zoneId()) == null, "tq-zoneid-null");

        ZonedDateTime z = ZonedDateTime.of(2026, 5, 21, 10, 30, 0, 0, ZoneId.of("America/New_York"));
        eq(z.query(TemporalQueries.zoneId()), ZoneId.of("America/New_York"), "tq-zdt-zone");
        eq(z.query(TemporalQueries.offset()), z.getOffset(), "tq-zdt-offset");

        // WeekFields.ISO: Monday=1 .. Sunday=7
        eqL(d.get(WeekFields.ISO.dayOfWeek()), 4, "wf-iso-dow"); // Thursday
        eqL(LocalDate.of(2026, 5, 25).get(WeekFields.ISO.dayOfWeek()), 1, "wf-iso-monday");
        eqL(LocalDate.of(2026, 5, 24).get(WeekFields.ISO.dayOfWeek()), 7, "wf-iso-sunday");
        check(WeekFields.ISO.getFirstDayOfWeek() == DayOfWeek.MONDAY, "wf-iso-firstday");
        eqL(WeekFields.ISO.getMinimalDaysInFirstWeek(), 4, "wf-iso-mindays");
    }

    // -------------------------------------------------------------- Chronology
    static void chronology() {
        check(IsoChronology.INSTANCE.isLeapYear(2024), "chrono-leap-2024");
        check(!IsoChronology.INSTANCE.isLeapYear(2026), "chrono-leap-2026");
        ChronoLocalDate cd = IsoChronology.INSTANCE.date(2026, 5, 21);
        eq(LocalDate.from(cd), LocalDate.of(2026, 5, 21), "chrono-date");
        eq(IsoChronology.INSTANCE.getId(), "ISO", "chrono-id");
        eqL(IsoChronology.INSTANCE.dateEpochDay(0).toEpochDay(), 0, "chrono-epochday");
        // chronology accessible from a date
        check(LocalDate.of(2026, 5, 21).getChronology() == IsoChronology.INSTANCE, "chrono-from-date");
    }

    // -------------------------------------------------------------- Exceptions
    static void exceptions() {
        expect(DateTimeException.class, () -> LocalDate.of(2026, 2, 30), "ex-feb30");
        expect(DateTimeException.class, () -> LocalDate.of(2026, 13, 1), "ex-month13");
        expect(DateTimeException.class, () -> LocalDate.of(2026, 5, 0), "ex-day0");
        expect(DateTimeException.class, () -> LocalTime.of(24, 0), "ex-hour24");
        expect(DateTimeException.class, () -> LocalTime.of(0, 60), "ex-min60");
        expect(DateTimeException.class, () -> ZoneOffset.ofHours(19), "ex-offset19");
        expect(DateTimeException.class, () -> MonthDay.of(2, 30), "ex-monthday-feb30");

        expect(DateTimeParseException.class, () -> LocalDate.parse("2026-13-01"), "ex-parse-bad-month");
        expect(DateTimeParseException.class, () -> LocalDate.parse("not-a-date"), "ex-parse-garbage");
        expect(DateTimeParseException.class, () -> Duration.parse("1H"), "ex-parse-bad-duration");
        expect(DateTimeParseException.class, () -> Period.parse("P1X"), "ex-parse-bad-period");
        expect(DateTimeParseException.class, () -> Instant.parse("2026-05-21"), "ex-parse-bad-instant");

        // querying an unsupported field
        expect(UnsupportedTemporalTypeException.class,
                () -> LocalDate.of(2026, 5, 21).get(ChronoField.HOUR_OF_DAY), "ex-unsupported-field");
        expect(UnsupportedTemporalTypeException.class,
                () -> LocalTime.of(10, 0).get(ChronoField.DAY_OF_MONTH), "ex-unsupported-field-time");

        // ZoneId of unknown region
        expect(ZoneRulesException.class, () -> ZoneId.of("Mars/Olympus"), "ex-unknown-zone");

        // overflow: arithmetic exceeds Long range -> ArithmeticException
        expect(ArithmeticException.class,
                () -> Duration.ofSeconds(Long.MAX_VALUE).plusSeconds(1), "ex-duration-overflow");
        // Instant out of supported range -> DateTimeException
        expect(DateTimeException.class, () -> Instant.MAX.plusSeconds(1), "ex-instant-overflow");
        // Period.between requires both LocalDate-compatible (NPE on null)
        expect(NullPointerException.class, () -> LocalDate.of(2026, 5, 21).plusDays(0).isBefore(null), "ex-npe-isbefore");
    }
}
