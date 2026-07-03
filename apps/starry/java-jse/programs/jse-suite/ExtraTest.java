import java.io.*;
import java.math.*;
import java.text.*;
import java.util.*;
import java.util.regex.*;
import java.util.zip.*;

/* DoD carpet: java.math(BigInteger/BigDecimal/MathContext/RoundingMode) + java.text(DecimalFormat/
 * NumberFormat/MessageFormat/ChoiceFormat/SimpleDateFormat/Collator/Normalizer) + java.util.regex
 * (Pattern/Matcher/flags/named groups/lookaround/append/results/split) + java.util.zip(GZIP/Deflater/
 * Inflater/CRC32/Adler32/Zip{In,Out}putStream/Checked streams) + java.io 序列化 + DataIO + 流装饰器.
 * 地毯级: 正常 + 边界 + 异常路径, 全部精确断言 (== / equals / 确定值). 全部确定性 + 离线. */
public class ExtraTest {
    static int ok = 0, fail = 0;

    static void check(boolean c, String n) {
        if (c) ok++;
        else { fail++; System.out.println("FAIL " + n); }
    }

    interface ThrowingRunnable { void run() throws Exception; }

    static void expectThrows(Class<? extends Throwable> ex, String n, ThrowingRunnable r) {
        try {
            r.run();
            fail++;
            System.out.println("FAIL " + n + " (no throw)");
        } catch (Throwable t) {
            if (ex.isInstance(t)) ok++;
            else { fail++; System.out.println("FAIL " + n + " (got " + t.getClass().getName() + ")"); }
        }
    }

    // ---- serialization fixtures ----
    static class Point implements Serializable {
        private static final long serialVersionUID = 1L;
        int x, y;
        transient int cached;
        String label;
        Point(int x, int y, String label) { this.x = x; this.y = y; this.label = label; this.cached = x + y; }
    }

    static class Ext implements Externalizable {
        int a;
        String b;
        public Ext() {}
        Ext(int a, String b) { this.a = a; this.b = b; }
        public void writeExternal(ObjectOutput o) throws IOException { o.writeInt(a); o.writeObject(b); }
        public void readExternal(ObjectInput i) throws IOException, ClassNotFoundException { a = i.readInt(); b = (String) i.readObject(); }
    }

    static class Custom implements Serializable {
        private static final long serialVersionUID = 2L;
        int v;
        int derived; // recomputed on read from a value written by writeObject
        Custom(int v) { this.v = v; this.derived = -1; }
        private void writeObject(ObjectOutputStream o) throws IOException {
            o.defaultWriteObject();
            o.writeInt(v * 10);
        }
        private void readObject(ObjectInputStream i) throws IOException, ClassNotFoundException {
            i.defaultReadObject();
            derived = i.readInt();
        }
    }

    static byte[] ser(Object o) throws IOException {
        ByteArrayOutputStream bos = new ByteArrayOutputStream();
        try (ObjectOutputStream oos = new ObjectOutputStream(bos)) { oos.writeObject(o); }
        return bos.toByteArray();
    }

    static Object deser(byte[] b) throws IOException, ClassNotFoundException {
        try (ObjectInputStream ois = new ObjectInputStream(new ByteArrayInputStream(b))) { return ois.readObject(); }
    }

    static int deflateLen(byte[] data, int level) {
        Deflater d = new Deflater(level);
        d.setInput(data);
        d.finish();
        byte[] buf = new byte[256];
        int total = 0;
        while (!d.finished()) total += d.deflate(buf);
        d.end();
        return total;
    }

    public static void main(String[] args) throws Exception {
        bigInteger();
        bigDecimal();
        text();
        regex();
        zip();
        serialization();
        dataAndStreams();

        System.out.println("EXTRA_RESULT ok=" + ok + " fail=" + fail);
        if (fail == 0) System.out.println("EXTRA_DONE");
    }

    // =========================== java.math.BigInteger ===========================
    static void bigInteger() {
        BigInteger f = BigInteger.ONE;
        for (int i = 1; i <= 20; i++) f = f.multiply(BigInteger.valueOf(i));
        check(f.toString().equals("2432902008176640000"), "bi-factorial20");
        check(BigInteger.TWO.pow(64).toString().equals("18446744073709551616"), "bi-pow2-64");
        check(BigInteger.TEN.pow(10).toString().equals("10000000000"), "bi-pow10-10");

        // basic arithmetic
        BigInteger a = BigInteger.valueOf(17), b = BigInteger.valueOf(5);
        check(a.add(b).intValue() == 22, "bi-add");
        check(a.subtract(b).intValue() == 12, "bi-subtract");
        check(a.multiply(b).intValue() == 85, "bi-multiply");
        check(a.divide(b).intValue() == 3, "bi-divide");
        BigInteger[] dr = a.divideAndRemainder(b);
        check(dr[0].intValue() == 3 && dr[1].intValue() == 2, "bi-divideAndRemainder");
        check(BigInteger.valueOf(-7).mod(BigInteger.valueOf(3)).intValue() == 2, "bi-mod-nonneg");
        check(BigInteger.valueOf(-7).remainder(BigInteger.valueOf(3)).intValue() == -1, "bi-remainder-signed");

        // gcd / lcm
        BigInteger g = BigInteger.valueOf(48).gcd(BigInteger.valueOf(36));
        check(g.intValue() == 12, "bi-gcd");
        BigInteger lcm = BigInteger.valueOf(48).multiply(BigInteger.valueOf(36)).divide(g);
        check(lcm.intValue() == 144, "bi-lcm");

        // sign / abs / negate / min / max / compareTo
        check(BigInteger.valueOf(-5).abs().intValue() == 5, "bi-abs");
        check(BigInteger.valueOf(5).negate().intValue() == -5, "bi-negate");
        check(BigInteger.valueOf(-5).signum() == -1, "bi-signum-neg");
        check(BigInteger.ZERO.signum() == 0, "bi-signum-zero");
        check(BigInteger.valueOf(3).max(BigInteger.valueOf(7)).intValue() == 7, "bi-max");
        check(BigInteger.valueOf(3).min(BigInteger.valueOf(7)).intValue() == 3, "bi-min");
        check(BigInteger.valueOf(3).compareTo(BigInteger.valueOf(7)) < 0, "bi-compareTo");

        // radix
        check(new BigInteger("ff", 16).intValue() == 255, "bi-radix-parse");
        check(BigInteger.valueOf(255).toString(16).equals("ff"), "bi-toString-16");
        check(BigInteger.valueOf(255).toString(2).equals("11111111"), "bi-toString-2");

        // bit operations
        check(BigInteger.valueOf(7).bitCount() == 3, "bi-bitCount");
        check(BigInteger.valueOf(255).bitLength() == 8, "bi-bitLength-255");
        check(BigInteger.valueOf(256).bitLength() == 9, "bi-bitLength-256");
        check(BigInteger.valueOf(12).getLowestSetBit() == 2, "bi-lowestSetBit");
        check(BigInteger.ONE.shiftLeft(4).intValue() == 16, "bi-shiftLeft");
        check(BigInteger.valueOf(256).shiftRight(4).intValue() == 16, "bi-shiftRight");
        check(BigInteger.valueOf(12).and(BigInteger.valueOf(10)).intValue() == 8, "bi-and");
        check(BigInteger.valueOf(12).or(BigInteger.valueOf(10)).intValue() == 14, "bi-or");
        check(BigInteger.valueOf(12).xor(BigInteger.valueOf(10)).intValue() == 6, "bi-xor");
        check(BigInteger.ZERO.not().intValue() == -1, "bi-not");
        check(BigInteger.valueOf(5).testBit(0) && !BigInteger.valueOf(5).testBit(1) && BigInteger.valueOf(5).testBit(2), "bi-testBit");
        check(BigInteger.valueOf(4).setBit(0).intValue() == 5, "bi-setBit");
        check(BigInteger.valueOf(5).clearBit(0).intValue() == 4, "bi-clearBit");
        check(BigInteger.valueOf(5).flipBit(1).intValue() == 7, "bi-flipBit");

        // primes
        check(BigInteger.valueOf(97).isProbablePrime(30), "bi-isProbablePrime-true");
        check(!BigInteger.valueOf(100).isProbablePrime(30), "bi-isProbablePrime-false");
        check(BigInteger.valueOf(97).nextProbablePrime().intValue() == 101, "bi-nextProbablePrime");

        // modular arithmetic
        BigInteger mp = BigInteger.valueOf(4).modPow(BigInteger.valueOf(13), BigInteger.valueOf(497));
        check(mp.equals(BigInteger.valueOf(4).pow(13).mod(BigInteger.valueOf(497))), "bi-modPow-crosscheck");
        check(BigInteger.valueOf(3).modInverse(BigInteger.valueOf(11)).intValue() == 4, "bi-modInverse");

        // sqrt (Java 9+)
        check(BigInteger.valueOf(100).sqrt().intValue() == 10, "bi-sqrt-exact");
        check(BigInteger.valueOf(99).sqrt().intValue() == 9, "bi-sqrt-floor");
        BigInteger big = new BigInteger("12345678901234567890");
        BigInteger r = big.sqrt();
        check(r.multiply(r).compareTo(big) <= 0 && r.add(BigInteger.ONE).pow(2).compareTo(big) > 0, "bi-sqrt-bignum");

        // exact conversions
        check(BigInteger.valueOf(123).longValueExact() == 123L, "bi-longValueExact");
        check(BigInteger.valueOf(123).intValueExact() == 123, "bi-intValueExact");

        // exception paths
        expectThrows(ArithmeticException.class, "bi-divide-by-zero", () -> BigInteger.TEN.divide(BigInteger.ZERO));
        expectThrows(ArithmeticException.class, "bi-modInverse-noncoprime", () -> BigInteger.valueOf(6).modInverse(BigInteger.valueOf(9)));
        expectThrows(ArithmeticException.class, "bi-intValueExact-overflow", () -> BigInteger.valueOf(Long.MAX_VALUE).pow(2).intValueExact());
        expectThrows(NumberFormatException.class, "bi-parse-bad", () -> new BigInteger("not-a-number"));
    }

    // =========================== java.math.BigDecimal ===========================
    static void bigDecimal() {
        BigDecimal a = new BigDecimal("1.10"), b = new BigDecimal("2.20");
        check(a.add(b).compareTo(new BigDecimal("3.30")) == 0, "bd-add");
        check(b.subtract(a).compareTo(new BigDecimal("1.10")) == 0, "bd-subtract");
        check(new BigDecimal("1.1").multiply(new BigDecimal("1.1")).toString().equals("1.21"), "bd-multiply");
        check(BigDecimal.ONE.divide(new BigDecimal("3"), 5, RoundingMode.HALF_UP).toString().equals("0.33333"), "bd-divide-scale");
        check(new BigDecimal("2").divide(new BigDecimal("3"), 5, RoundingMode.HALF_UP).toString().equals("0.66667"), "bd-divide-roundup");

        // scale / precision / unscaledValue
        check(a.scale() == 2, "bd-scale");
        check(new BigDecimal("123.45").precision() == 5, "bd-precision");
        check(a.unscaledValue().intValue() == 110, "bd-unscaledValue");
        check(new BigDecimal("3.99").toBigInteger().intValue() == 3, "bd-toBigInteger");

        // compareTo vs equals (scale sensitivity)
        check(new BigDecimal("1.0").compareTo(new BigDecimal("1.00")) == 0, "bd-compareTo-scale");
        check(!new BigDecimal("1.0").equals(new BigDecimal("1.00")), "bd-equals-scale-sensitive");

        // stripTrailingZeros (incl. negative scale)
        check(new BigDecimal("1.2000").stripTrailingZeros().toPlainString().equals("1.2"), "bd-strip-1");
        check(new BigDecimal("1.000").stripTrailingZeros().toPlainString().equals("1"), "bd-strip-2");
        check(new BigDecimal("600").stripTrailingZeros().scale() == -2, "bd-strip-negscale");

        // movePoint
        check(new BigDecimal("123").movePointLeft(2).toString().equals("1.23"), "bd-movePointLeft");
        check(new BigDecimal("1.23").movePointRight(2).toString().equals("123"), "bd-movePointRight");

        // setScale rounding modes
        check(new BigDecimal("2.345").setScale(2, RoundingMode.HALF_UP).toString().equals("2.35"), "bd-HALF_UP");
        check(new BigDecimal("2.345").setScale(2, RoundingMode.HALF_DOWN).toString().equals("2.34"), "bd-HALF_DOWN");
        check(new BigDecimal("2.345").setScale(2, RoundingMode.HALF_EVEN).toString().equals("2.34"), "bd-HALF_EVEN-even");
        check(new BigDecimal("2.355").setScale(2, RoundingMode.HALF_EVEN).toString().equals("2.36"), "bd-HALF_EVEN-odd");
        check(new BigDecimal("2.344").setScale(2, RoundingMode.UP).toString().equals("2.35"), "bd-UP");
        check(new BigDecimal("2.346").setScale(2, RoundingMode.DOWN).toString().equals("2.34"), "bd-DOWN");
        check(new BigDecimal("2.341").setScale(2, RoundingMode.CEILING).toString().equals("2.35"), "bd-CEILING-pos");
        check(new BigDecimal("2.349").setScale(2, RoundingMode.FLOOR).toString().equals("2.34"), "bd-FLOOR-pos");
        check(new BigDecimal("-2.341").setScale(2, RoundingMode.CEILING).toString().equals("-2.34"), "bd-CEILING-neg");
        check(new BigDecimal("-2.341").setScale(2, RoundingMode.FLOOR).toString().equals("-2.35"), "bd-FLOOR-neg");

        // pow / remainder / divideToIntegralValue / abs / negate / signum / plus
        check(new BigDecimal("2").pow(3).toString().equals("8"), "bd-pow");
        check(new BigDecimal("10").remainder(new BigDecimal("3")).toString().equals("1"), "bd-remainder");
        check(new BigDecimal("10").divideToIntegralValue(new BigDecimal("3")).toString().equals("3"), "bd-divideToIntegral");
        check(new BigDecimal("-2.5").abs().toString().equals("2.5"), "bd-abs");
        check(new BigDecimal("2.5").negate().toString().equals("-2.5"), "bd-negate");
        check(new BigDecimal("-2.5").signum() == -1, "bd-signum");
        check(new BigDecimal("3.7").max(new BigDecimal("3.6")).toString().equals("3.7"), "bd-max");

        // MathContext
        check(new BigDecimal("123.456").round(new MathContext(4, RoundingMode.HALF_UP)).toString().equals("123.5"), "bd-round-mc4");
        check(new BigDecimal("123.456").round(new MathContext(2, RoundingMode.HALF_UP)).compareTo(new BigDecimal("120")) == 0, "bd-round-mc2");
        check(BigDecimal.ONE.divide(new BigDecimal("3"), new MathContext(5)).toString().equals("0.33333"), "bd-divide-mc");

        // valueOf(double) vs new BigDecimal(double)
        check(BigDecimal.valueOf(0.1).toString().equals("0.1"), "bd-valueOf-double");
        check(new BigDecimal(0.1).compareTo(BigDecimal.valueOf(0.1)) != 0, "bd-new-double-inexact");

        // exception paths
        expectThrows(ArithmeticException.class, "bd-divide-nonterminating", () -> BigDecimal.ONE.divide(new BigDecimal("3")));
        expectThrows(ArithmeticException.class, "bd-setScale-unnecessary", () -> new BigDecimal("2.345").setScale(2, RoundingMode.UNNECESSARY));
        expectThrows(NumberFormatException.class, "bd-parse-bad", () -> new BigDecimal("xyz"));
    }

    // =========================== java.text ===========================
    static void text() throws Exception {
        DecimalFormatSymbols us = new DecimalFormatSymbols(Locale.US);

        // DecimalFormat: grouping + fraction
        DecimalFormat df = new DecimalFormat("#,##0.00", us);
        check(df.format(1234567.891).equals("1,234,567.89"), "df-grouping-frac");
        check(new DecimalFormat("#,##0", us).format(1234567).equals("1,234,567"), "df-grouping-int");

        // parse (returns Number)
        Number parsed = df.parse("1,234,567.89", new ParsePosition(0));
        check(Math.abs(parsed.doubleValue() - 1234567.89) < 1e-9, "df-parse");

        // negative subpattern
        check(new DecimalFormat("#,##0.00;(#,##0.00)", us).format(-1234.5).equals("(1,234.50)"), "df-negative-subpattern");

        // scientific
        check(new DecimalFormat("0.00E0", us).format(12345.0).equals("1.23E4"), "df-scientific");

        // setMaximum/MinimumFractionDigits + rounding mode
        DecimalFormat dyn = new DecimalFormat("0.###", us);
        check(dyn.format(1.23456).equals("1.235"), "df-maxfrac-default");
        dyn.setMaximumFractionDigits(2);
        check(dyn.format(1.23456).equals("1.23"), "df-setMaxFrac");
        dyn.setMinimumFractionDigits(2);
        check(dyn.format(1.0).equals("1.00"), "df-setMinFrac");
        DecimalFormat floorFmt = new DecimalFormat("0.0", us);
        floorFmt.setRoundingMode(RoundingMode.FLOOR);
        check(floorFmt.format(1.999).equals("1.9"), "df-roundingMode-floor");

        // custom DecimalFormatSymbols
        DecimalFormatSymbols custom = new DecimalFormatSymbols(Locale.US);
        custom.setGroupingSeparator('_');
        check(new DecimalFormat("#,##0", custom).format(1234567).equals("1_234_567"), "df-custom-symbols");

        // NumberFormat factory methods (Locale.US)
        check(NumberFormat.getPercentInstance(Locale.US).format(0.25).equals("25%"), "nf-percent");
        check(NumberFormat.getCurrencyInstance(Locale.US).format(1234.5).equals("$1,234.50"), "nf-currency");
        check(NumberFormat.getIntegerInstance(Locale.US).format(1234.9).equals("1,235"), "nf-integer-rounds");
        check(NumberFormat.getNumberInstance(Locale.US).format(1234.5).equals("1,234.5"), "nf-number");

        // MessageFormat
        check(MessageFormat.format("{0}+{1}={2}", 1, 2, 3).equals("1+2=3"), "mf-basic");
        check(new MessageFormat("{0,number,#,##0.0}", Locale.US).format(new Object[]{1234.5}).equals("1,234.5"), "mf-number-arg");
        check(new MessageFormat("{0,choice,0#none|1#one|1<many}", Locale.US).format(new Object[]{5}).equals("many"), "mf-choice");

        // ChoiceFormat
        ChoiceFormat cf = new ChoiceFormat(new double[]{0, 1, 2}, new String[]{"none", "one", "many"});
        check(cf.format(0).equals("none"), "cf-0");
        check(cf.format(1).equals("one"), "cf-1");
        check(cf.format(7).equals("many"), "cf-many");

        // SimpleDateFormat (deterministic: UTC + Locale.US)
        TimeZone utc = TimeZone.getTimeZone("UTC");
        SimpleDateFormat sdf = new SimpleDateFormat("yyyy-MM-dd HH:mm:ss", Locale.US);
        sdf.setTimeZone(utc);
        check(sdf.format(new Date(0L)).equals("1970-01-01 00:00:00"), "sdf-format-epoch");
        check(sdf.parse("2000-01-01 00:00:00").getTime() == 946684800000L, "sdf-parse");
        SimpleDateFormat sdf2 = new SimpleDateFormat("EEE, MMM d", Locale.US);
        sdf2.setTimeZone(utc);
        check(sdf2.format(new Date(0L)).equals("Thu, Jan 1"), "sdf-weekday");

        // Collator (Locale.US) — ordering + strength
        Collator coll = Collator.getInstance(Locale.US);
        check(coll.compare("apple", "banana") < 0, "coll-order");
        check(coll.compare("banana", "apple") > 0, "coll-order-rev");
        coll.setStrength(Collator.PRIMARY);
        check(coll.compare("ABC", "abc") == 0, "coll-primary-case");
        check(coll.compare("café", "cafe") == 0, "coll-primary-accent");
        Collator c2 = Collator.getInstance(Locale.US);
        CollationKey k1 = c2.getCollationKey("apple"), k2 = c2.getCollationKey("banana");
        check(k1.compareTo(k2) < 0, "coll-key");

        // Normalizer
        String decomposed = "é"; // e + combining acute accent
        String nfc = Normalizer.normalize(decomposed, Normalizer.Form.NFC);
        check(nfc.equals("é") && nfc.length() == 1, "norm-nfc");
        check(Normalizer.normalize("é", Normalizer.Form.NFD).length() == 2, "norm-nfd");
        check(Normalizer.isNormalized("é", Normalizer.Form.NFC), "norm-isNormalized-true");
        check(!Normalizer.isNormalized(decomposed, Normalizer.Form.NFC), "norm-isNormalized-false");

        // ParseException path
        expectThrows(ParseException.class, "df-parse-bad", () -> new DecimalFormat("0", us).parse("abc"));
    }

    // =========================== java.util.regex ===========================
    static void regex() {
        // capture groups + indices
        Matcher m = Pattern.compile("(\\d{4})-(\\d{2})-(\\d{2})").matcher("date 2026-05-21 end");
        check(m.find(), "rx-find");
        check(m.group(1).equals("2026") && m.group(2).equals("05") && m.group(3).equals("21"), "rx-groups");
        check(m.group(0).equals("2026-05-21"), "rx-group0");
        check(m.groupCount() == 3, "rx-groupCount");
        check(m.start() == 5 && m.end() == 15, "rx-start-end");
        check(m.start(1) == 5 && m.end(3) == 15, "rx-group-indices");

        // named groups
        Matcher nm = Pattern.compile("(?<y>\\d{4})-(?<m>\\d{2})").matcher("2026-05");
        check(nm.matches() && nm.group("y").equals("2026") && nm.group("m").equals("05"), "rx-named-groups");

        // matches vs lookingAt vs find
        Pattern digits = Pattern.compile("\\d+");
        check(!digits.matcher("123abc").matches(), "rx-matches-false");
        check(digits.matcher("123abc").lookingAt(), "rx-lookingAt");
        check(digits.matcher("abc123").find(), "rx-find-mid");
        check(Pattern.matches("\\d+", "98765"), "rx-static-matches");

        // replace
        check("a1b2c3".replaceAll("\\d", "#").equals("a#b#c#"), "rx-replaceAll");
        check("a1b2c3".replaceFirst("\\d", "#").equals("a#b2c3"), "rx-replaceFirst");
        check(Pattern.compile("(\\w)(\\d)").matcher("a1b2").replaceAll("$2$1").equals("1a2b"), "rx-replace-backref");

        // replaceAll(Function) Java 9+
        check(Pattern.compile("\\d").matcher("a1b2").replaceAll(r -> "<" + r.group() + ">").equals("a<1>b<2>"), "rx-replace-fn");

        // appendReplacement / appendTail
        Matcher am = Pattern.compile("\\d+").matcher("a1b22c333");
        StringBuffer sb = new StringBuffer();
        while (am.find()) am.appendReplacement(sb, "#");
        am.appendTail(sb);
        check(sb.toString().equals("a#b#c#"), "rx-appendReplacement");

        // results() stream (Java 9+)
        check(Pattern.compile("\\d+").matcher("1 22 333").results().count() == 3, "rx-results-count");

        // flags
        check(Pattern.compile("abc", Pattern.CASE_INSENSITIVE).matcher("ABC").matches(), "rx-case-insensitive");
        check(Pattern.compile("a.b", Pattern.DOTALL).matcher("a\nb").matches(), "rx-dotall");
        check(!Pattern.compile("a.b").matcher("a\nb").matches(), "rx-dotall-off");
        check(Pattern.compile("^\\w+", Pattern.MULTILINE).matcher("foo\nbar").results().count() == 2, "rx-multiline");
        check(Pattern.compile("(?i)hello").matcher("HELLO").matches(), "rx-inline-flag");

        // lookaround
        check("foobar".replaceAll("foo(?=bar)", "X").equals("Xbar"), "rx-lookahead");
        check("foobar".replaceAll("(?<=foo)bar", "X").equals("fooX"), "rx-lookbehind");

        // backreference
        Matcher bref = Pattern.compile("(\\w)\\1").matcher("hello");
        check(bref.find() && bref.group(0).equals("ll"), "rx-backreference");

        // greedy vs lazy
        Matcher greedy = Pattern.compile("a.+b").matcher("axbxb");
        check(greedy.find() && greedy.group().equals("axbxb"), "rx-greedy");
        Matcher lazy = Pattern.compile("a.+?b").matcher("axbxb");
        check(lazy.find() && lazy.group().equals("axb"), "rx-lazy");

        // quote
        check(Pattern.compile(Pattern.quote("a.b")).matcher("a.b").matches(), "rx-quote-match");
        check(!Pattern.compile(Pattern.quote("a.b")).matcher("axb").matches(), "rx-quote-nomatch");

        // split with limits
        check(Pattern.compile(",").split("x,y,z").length == 3, "rx-split");
        check(Pattern.compile(",").split("a,b,c,,").length == 3, "rx-split-trailing-removed");
        check(Pattern.compile(",").split("a,b,c,,", -1).length == 5, "rx-split-trailing-kept");
        check(Pattern.compile(",").split("a,b,c", 2)[1].equals("b,c"), "rx-split-limit");

        // reset / region
        Matcher rm = Pattern.compile("\\d").matcher("1a2");
        check(rm.find() && rm.find() && !rm.find(), "rx-iterate");
        rm.reset();
        check(rm.find() && rm.group().equals("1"), "rx-reset");

        // PatternSyntaxException
        expectThrows(PatternSyntaxException.class, "rx-bad-pattern", () -> Pattern.compile("(unclosed"));
    }

    // =========================== java.util.zip ===========================
    static void zip() throws Exception {
        byte[] data = "the quick brown fox ".repeat(50).getBytes("UTF-8");

        // GZIP roundtrip
        ByteArrayOutputStream bo = new ByteArrayOutputStream();
        try (GZIPOutputStream gz = new GZIPOutputStream(bo)) { gz.write(data); }
        byte[] comp = bo.toByteArray();
        check(comp.length < data.length, "zip-gzip-smaller");
        byte[] decomp = new GZIPInputStream(new ByteArrayInputStream(comp)).readAllBytes();
        check(Arrays.equals(decomp, data), "zip-gzip-roundtrip");

        // Deflater / Inflater raw roundtrip + counters
        Deflater def = new Deflater(Deflater.BEST_COMPRESSION);
        def.setInput(data);
        def.finish();
        ByteArrayOutputStream cbo = new ByteArrayOutputStream();
        byte[] buf = new byte[256];
        while (!def.finished()) cbo.write(buf, 0, def.deflate(buf));
        long bytesRead = def.getBytesRead();
        def.end();
        byte[] raw = cbo.toByteArray();
        check(raw.length < data.length, "zip-deflate-smaller");
        check(bytesRead == data.length, "zip-deflate-bytesRead");

        Inflater inf = new Inflater();
        inf.setInput(raw);
        ByteArrayOutputStream dbo = new ByteArrayOutputStream();
        byte[] dbuf = new byte[256];
        while (!inf.finished()) {
            int n = inf.inflate(dbuf);
            if (n == 0 && inf.finished()) break;
            dbo.write(dbuf, 0, n);
        }
        long written = inf.getBytesWritten();
        inf.end();
        check(Arrays.equals(dbo.toByteArray(), data), "zip-inflate-roundtrip");
        check(written == data.length, "zip-inflate-bytesWritten");

        // compression levels
        byte[] data2 = "abcdefgh".repeat(100).getBytes("UTF-8");
        check(deflateLen(data2, Deflater.BEST_COMPRESSION) < deflateLen(data2, Deflater.NO_COMPRESSION), "zip-level-compare");

        // Deflater with preset dictionary
        byte[] dict = "brown fox".getBytes("UTF-8");
        Deflater dd = new Deflater();
        dd.setDictionary(dict);
        dd.setInput(data);
        dd.finish();
        ByteArrayOutputStream ddo = new ByteArrayOutputStream();
        while (!dd.finished()) ddo.write(buf, 0, dd.deflate(buf));
        dd.end();
        byte[] dcomp = ddo.toByteArray();
        Inflater di = new Inflater();
        di.setInput(dcomp);
        ByteArrayOutputStream dio = new ByteArrayOutputStream();
        while (!di.finished()) {
            int n = di.inflate(dbuf);
            if (n == 0) {
                if (di.needsDictionary()) di.setDictionary(dict);
                else if (di.finished()) break;
                else if (di.needsInput()) break;
            } else {
                dio.write(dbuf, 0, n);
            }
        }
        di.end();
        check(Arrays.equals(dio.toByteArray(), data), "zip-deflate-dictionary");

        // CRC32 — canonical check value of "123456789" is 0xCBF43926
        CRC32 crc = new CRC32();
        crc.update("123456789".getBytes("US-ASCII"));
        check(crc.getValue() == 0xCBF43926L, "zip-crc32-canonical");
        CRC32 crcEmpty = new CRC32();
        check(crcEmpty.getValue() == 0L, "zip-crc32-empty");
        // incremental update equals whole-buffer update
        CRC32 cWhole = new CRC32(); cWhole.update(data);
        CRC32 cByte = new CRC32(); for (byte x : data) cByte.update(x);
        check(cWhole.getValue() == cByte.getValue(), "zip-crc32-incremental");
        crc.reset();
        check(crc.getValue() == 0L, "zip-crc32-reset");

        // Adler32 — "abc" = 38600999
        Adler32 ad = new Adler32();
        ad.update("abc".getBytes("US-ASCII"));
        check(ad.getValue() == 38600999L, "zip-adler32-abc");
        Adler32 adInit = new Adler32();
        check(adInit.getValue() == 1L, "zip-adler32-init");

        // CheckedOutputStream / CheckedInputStream
        CheckedOutputStream cos = new CheckedOutputStream(new ByteArrayOutputStream(), new CRC32());
        cos.write("123456789".getBytes("US-ASCII"));
        check(cos.getChecksum().getValue() == 0xCBF43926L, "zip-checkedoutput");
        CheckedInputStream cis = new CheckedInputStream(new ByteArrayInputStream("123456789".getBytes("US-ASCII")), new CRC32());
        cis.readAllBytes();
        check(cis.getChecksum().getValue() == 0xCBF43926L, "zip-checkedinput");

        // ZipOutputStream / ZipInputStream with multiple entries
        ByteArrayOutputStream zbo = new ByteArrayOutputStream();
        try (ZipOutputStream zos = new ZipOutputStream(zbo)) {
            zos.putNextEntry(new ZipEntry("a.txt"));
            zos.write("alpha".getBytes("UTF-8"));
            zos.closeEntry();
            zos.putNextEntry(new ZipEntry("dir/b.txt"));
            zos.write("beta".getBytes("UTF-8"));
            zos.closeEntry();
        }
        ZipInputStream zis = new ZipInputStream(new ByteArrayInputStream(zbo.toByteArray()));
        ZipEntry e1 = zis.getNextEntry();
        check(e1 != null && e1.getName().equals("a.txt"), "zip-entry1-name");
        check(new String(zis.readAllBytes(), "UTF-8").equals("alpha"), "zip-entry1-content");
        ZipEntry e2 = zis.getNextEntry();
        check(e2 != null && e2.getName().equals("dir/b.txt"), "zip-entry2-name");
        check(new String(zis.readAllBytes(), "UTF-8").equals("beta"), "zip-entry2-content");
        check(zis.getNextEntry() == null, "zip-entry-end");
        zis.close();

        // DeflaterOutputStream / InflaterInputStream
        ByteArrayOutputStream dfo = new ByteArrayOutputStream();
        try (DeflaterOutputStream dos = new DeflaterOutputStream(dfo)) { dos.write(data); }
        byte[] back = new InflaterInputStream(new ByteArrayInputStream(dfo.toByteArray())).readAllBytes();
        check(Arrays.equals(back, data), "zip-deflaterstream-roundtrip");

        // corrupt inflate -> DataFormatException
        expectThrows(DataFormatException.class, "zip-inflate-corrupt", () -> {
            Inflater bad = new Inflater();
            bad.setInput(new byte[]{(byte) 0xFF, (byte) 0xFF, (byte) 0xFF, (byte) 0xFF});
            bad.inflate(new byte[16]);
        });
    }

    // =========================== java.io serialization ===========================
    static void serialization() throws Exception {
        // basic object + transient
        Point p = new Point(3, 4, "origin");
        Point rp = (Point) deser(ser(p));
        check(rp.x == 3 && rp.y == 4 && rp.label.equals("origin"), "ser-point-fields");
        check(rp.cached == 0, "ser-transient-not-restored");

        // collections / maps
        @SuppressWarnings("unchecked")
        Map<String, Integer> rm = (Map<String, Integer>) deser(ser(new HashMap<>(Map.of("k", 42, "x", 7))));
        check(rm.get("k") == 42 && rm.get("x") == 7, "ser-map");
        @SuppressWarnings("unchecked")
        List<String> rl = (List<String>) deser(ser(new ArrayList<>(List.of("a", "b", "c"))));
        check(rl.equals(List.of("a", "b", "c")), "ser-list");

        // arrays
        int[] arr = {1, 2, 3, 4, 5};
        check(Arrays.equals((int[]) deser(ser(arr)), arr), "ser-int-array");
        String[] sarr = {"x", "y"};
        check(Arrays.equals((String[]) deser(ser(sarr)), sarr), "ser-string-array");

        // Externalizable
        Ext rex = (Ext) deser(ser(new Ext(99, "ext")));
        check(rex.a == 99 && rex.b.equals("ext"), "ser-externalizable");

        // custom writeObject/readObject
        Custom rc = (Custom) deser(ser(new Custom(5)));
        check(rc.v == 5 && rc.derived == 50, "ser-custom-readwrite");

        // nested object graph in one stream, ordered reads
        ByteArrayOutputStream bos = new ByteArrayOutputStream();
        try (ObjectOutputStream oos = new ObjectOutputStream(bos)) {
            oos.writeInt(7);
            oos.writeUTF("tag");
            oos.writeObject(new Point(1, 1, "a"));
            oos.writeObject(List.of(10, 20));
        }
        try (ObjectInputStream ois = new ObjectInputStream(new ByteArrayInputStream(bos.toByteArray()))) {
            check(ois.readInt() == 7, "ser-stream-int");
            check(ois.readUTF().equals("tag"), "ser-stream-utf");
            check(((Point) ois.readObject()).label.equals("a"), "ser-stream-object");
            @SuppressWarnings("unchecked")
            List<Integer> li = (List<Integer>) ois.readObject();
            check(li.equals(List.of(10, 20)), "ser-stream-list");
        }

        // reference sharing preserved (same instance written twice -> identical reference on read)
        Point shared = new Point(8, 9, "s");
        ByteArrayOutputStream sbos = new ByteArrayOutputStream();
        try (ObjectOutputStream oos = new ObjectOutputStream(sbos)) {
            oos.writeObject(shared);
            oos.writeObject(shared);
        }
        try (ObjectInputStream ois = new ObjectInputStream(new ByteArrayInputStream(sbos.toByteArray()))) {
            Object o1 = ois.readObject(), o2 = ois.readObject();
            check(o1 == o2, "ser-reference-sharing");
        }

        // reading past end -> EOFException
        expectThrows(EOFException.class, "ser-eof", () -> {
            try (ObjectInputStream ois = new ObjectInputStream(new ByteArrayInputStream(ser("done")))) {
                ois.readObject();
                ois.readObject();
            }
        });
    }

    // =========================== java.io DataIO + stream decorators ===========================
    static void dataAndStreams() throws Exception {
        // DataOutputStream / DataInputStream
        ByteArrayOutputStream bos = new ByteArrayOutputStream();
        DataOutputStream dos = new DataOutputStream(bos);
        dos.writeInt(0x01020304);
        dos.writeUTF("héllo");
        dos.writeDouble(3.14);
        dos.writeBoolean(true);
        dos.writeLong(0x0102030405060708L);
        dos.writeShort(0x1234);
        dos.writeByte(0xAB);
        dos.flush();
        check(dos.size() == 32, "io-dos-size");
        DataInputStream dis = new DataInputStream(new ByteArrayInputStream(bos.toByteArray()));
        check(dis.readInt() == 0x01020304, "io-readInt");
        check(dis.readUTF().equals("héllo"), "io-readUTF");
        check(dis.readDouble() == 3.14, "io-readDouble");
        check(dis.readBoolean(), "io-readBoolean");
        check(dis.readLong() == 0x0102030405060708L, "io-readLong");
        check(dis.readShort() == 0x1234, "io-readShort");
        check(dis.readUnsignedByte() == 0xAB, "io-readUnsignedByte");

        // PushbackInputStream
        PushbackInputStream pb = new PushbackInputStream(new ByteArrayInputStream(new byte[]{1, 2, 3}), 4);
        int first = pb.read();
        check(first == 1, "io-pushback-read");
        pb.unread(first);
        check(pb.read() == 1 && pb.read() == 2, "io-pushback-unread");

        // ByteArrayInputStream mark/reset
        ByteArrayInputStream bis = new ByteArrayInputStream(new byte[]{10, 20, 30});
        bis.read();
        bis.mark(10);
        int v = bis.read();
        bis.reset();
        check(bis.read() == v && v == 20, "io-mark-reset");

        // SequenceInputStream
        InputStream seq = new SequenceInputStream(
                new ByteArrayInputStream(new byte[]{1, 2}),
                new ByteArrayInputStream(new byte[]{3, 4}));
        check(Arrays.equals(seq.readAllBytes(), new byte[]{1, 2, 3, 4}), "io-sequence");

        // BufferedReader over StringReader (line iteration)
        BufferedReader br = new BufferedReader(new StringReader("one\ntwo\nthree"));
        check(br.readLine().equals("one"), "io-bufferedreader-line1");
        check(br.lines().count() == 2, "io-bufferedreader-lines");

        // PrintWriter -> StringWriter
        StringWriter sw = new StringWriter();
        PrintWriter pw = new PrintWriter(sw);
        pw.printf("%d-%s", 42, "x");
        pw.flush();
        check(sw.toString().equals("42-x"), "io-printwriter-printf");

        // CharArrayWriter
        CharArrayWriter caw = new CharArrayWriter();
        caw.write("abc");
        caw.append('d');
        check(caw.toString().equals("abcd") && caw.size() == 4, "io-chararraywriter");

        // LineNumberReader
        LineNumberReader lnr = new LineNumberReader(new StringReader("a\nb\nc"));
        lnr.readLine();
        lnr.readLine();
        check(lnr.getLineNumber() == 2, "io-linenumberreader");

        // StreamTokenizer
        StreamTokenizer st = new StreamTokenizer(new StringReader("12 word"));
        st.nextToken();
        boolean num = st.ttype == StreamTokenizer.TT_NUMBER && st.nval == 12.0;
        st.nextToken();
        boolean word = st.ttype == StreamTokenizer.TT_WORD && st.sval.equals("word");
        check(num && word, "io-streamtokenizer");

        // PipedInputStream/PipedOutputStream (single thread, write then read)
        PipedOutputStream pos = new PipedOutputStream();
        PipedInputStream pis = new PipedInputStream(pos, 16);
        pos.write(new byte[]{7, 8, 9});
        pos.close();
        check(Arrays.equals(pis.readAllBytes(), new byte[]{7, 8, 9}), "io-piped");
    }
}
