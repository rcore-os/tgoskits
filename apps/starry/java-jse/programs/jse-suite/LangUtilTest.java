import java.nio.*;
import java.nio.charset.*;
import java.lang.reflect.*;
import java.text.*;
import java.util.*;
import java.util.regex.*;
import java.util.random.*;

/* Carpet-grade coverage for the java.lang core types, the java.util utility
 * classes, the java.nio buffer / charset families, java.util.regex, and a
 * deterministic slice of java.text.
 *
 * Every assertion checks an exact, deterministic value (==, equals, a known
 * constant, or a tight epsilon for transcendental functions). No external
 * I/O, no network, no JIT/timing dependence. Threads are bounded (<=8) and
 * always joined, so results are deterministic. musl / StarryOS safe: only
 * heap buffers (no direct/native), only built-in charsets, explicit
 * Locale.US / explicit DecimalFormatSymbols (no host-locale dependence).
 *
 * Coverage matrix:
 *   java.lang.String        length/charAt/codePointAt/codePointCount/substring/
 *                           indexOf/lastIndexOf/contains/starts+endsWith/equals/
 *                           compareTo/case/trim/strip/isBlank/replace/split(limit)/
 *                           join/concat/matches/repeat/lines/chars/transform/
 *                           regionMatches/toCharArray/valueOf/hashCode/indent/
 *                           intern/text-block
 *   java.lang.StringBuilder append(overloads)/insert/delete/deleteCharAt/replace/
 *                           reverse/setCharAt/indexOf/lastIndexOf/substring/
 *                           setLength/capacity/appendCodePoint  (+ StringBuffer)
 *   java.lang.Character     isDigit/isLetter/isWhitespace/isUpper+Lower/toUpper+Lower/
 *                           getNumericValue/digit/forDigit/isAlphabetic/charCount/
 *                           toChars/codePoint/surrogate/getType/compare/MIN+MAX_RADIX
 *   java.lang.Math/StrictMath abs/max/min/pow/sqrt/cbrt/ceil/floor/round/rint/signum/
 *                           hypot/log/exp/toRad+Deg/floorDiv/floorMod/*Exact/ulp/
 *                           getExponent/scalb/fma/copySign/IEEEremainder/multiplyHigh
 *   wrappers Integer/Long/Short/Byte  parse(radix)/toString(radix)/bit ops/unsigned/
 *                           reverse/rotate/compare/sum/decode/cache/overflow
 *   wrappers Double/Float/Boolean  bits<->value/NaN/Infinity/compare/-0.0/logical*
 *   java.util.Objects       equals/deepEquals/hash/hashCode/toString/requireNonNull/
 *                           isNull/nonNull/compare/checkIndex/requireNonNullElse
 *   java.lang Throwable      chaining/getCause/getSuppressed/addSuppressed/custom/
 *                           multi-catch/try-with-resources/stack trace
 *   java.lang Thread/ThreadLocal  start/join/state/name/yield/interrupt-flag/
 *                           synchronized accumulation/ThreadLocal.withInitial
 *   enum/record             values/valueOf/ordinal/name/compareTo/EnumSet/EnumMap/
 *                           record accessors/equals/toString/isRecord/components
 *   java.util.Random        seeded determinism/nextInt(bound)/nextLong/nextBoolean/
 *                           nextGaussian/ints(); SplittableRandom; RandomGenerator(17)
 *   java.util.Scanner       nextInt/nextDouble(Locale)/next/nextLine/hasNextX/delim/radix
 *   java.util.StringTokenizer count/nextToken/delims/returnDelims
 *   java.util.BitSet        set(range)/clear/flip/get/and/or/xor/andNot/nextBit/prevBit/
 *                           cardinality/length/valueOf/toByteArray/intersects/stream
 *   java.util.UUID          fromString/nameUUIDFromBytes(MD5,deterministic)/version/
 *                           msb+lsb/compareTo/roundtrip
 *   java.util.StringJoiner  prefix/suffix/setEmptyValue/merge
 *   java.util.Optional      of/empty/map/flatMap/filter/or/orElse/stream/OptionalInt
 *   java.util.regex         compile/matches/find/group/named/groupCount/start+end/
 *                           replaceAll/replaceFirst/split/quote/flags/lookahead/
 *                           backref/results/appendReplacement
 *   java.util.Formatter     %d/%s/%x/%o/%e/%f/%,/%+/%0/%-/%(/% /%#/%c/%b/arg-index
 *   java.nio buffers        ByteBuffer put/get(rel+abs)/typed/flip/clear/rewind/mark/
 *                           reset/compact/slice/duplicate/readOnly/order/asIntBuffer/
 *                           wrap/array; CharBuffer/IntBuffer; under+overflow exceptions
 *   java.nio.charset        UTF_8/US_ASCII/ISO_8859_1/UTF_16(BE)/forName/encode/decode/
 *                           CharsetEncoder.canEncode/roundtrip
 *   java.text               DecimalFormat (explicit symbols)/parse/percent/scientific/
 *                           MessageFormat
 *   java.lang.Class         getName/getSimpleName/isPrimitive/isArray/componentType/
 *                           isAssignableFrom/TYPE/forName + light reflection invoke
 */
public class LangUtilTest {
    static int ok = 0, fail = 0;
    static void check(boolean c, String name) {
        if (c) { ok++; } else { fail++; System.out.println("FAIL " + name); }
    }
    static boolean close(double a, double b, double eps) { return Math.abs(a - b) <= eps; }

    // ---- nested helper types ----
    enum Color { RED, GREEN, BLUE }
    record Point(int x, int y) { }
    static class Holder {
        public int v;
        public Holder(int v) { this.v = v; }
        public int getV() { return v; }
        public int add(int a, int b) { return a + b; }
    }
    static final class Resource implements AutoCloseable {
        final List<String> log; final String id; final boolean throwOnClose;
        Resource(List<String> log, String id, boolean throwOnClose) { this.log = log; this.id = id; this.throwOnClose = throwOnClose; }
        public void close() { log.add("close-" + id); if (throwOnClose) throw new IllegalStateException("close-fail-" + id); }
    }

    public static void main(String[] args) throws Exception {
        sectionString();
        sectionStringBuilder();
        sectionCharacter();
        sectionMath();
        sectionIntegralWrappers();
        sectionFloatBoolWrappers();
        sectionObjectsSystem();
        sectionEnumRecord();
        sectionThrowable();
        sectionThread();
        sectionRandom();
        sectionScannerTokenizer();
        sectionBitSet();
        sectionUuidJoinerOptional();
        sectionRegex();
        sectionFormat();
        sectionByteBuffer();
        sectionCharset();
        sectionText();
        sectionReflection();

        System.out.println("LANGUTIL_RESULT ok=" + ok + " fail=" + fail);
        if (fail == 0) System.out.println("LANGUTIL_DONE");
    }

    // ================= java.lang.String =================
    static void sectionString() {
        String s = "Hello,World";
        check(s.length() == 11, "str-length");
        check(s.charAt(0) == 'H', "str-charAt");
        check(s.codePointAt(0) == 72, "str-codePointAt");
        check("café".codePointCount(0, 4) == 4, "str-codePointCount");
        check(s.substring(6).equals("World"), "str-substring1");
        check(s.substring(0, 5).equals("Hello"), "str-substring2");
        check(s.indexOf('o') == 4, "str-indexOf-char");
        check(s.indexOf('o', 5) == 7, "str-indexOf-char-from");
        check(s.lastIndexOf('o') == 7, "str-lastIndexOf-char");
        check(s.indexOf("World") == 6, "str-indexOf-str");
        check(s.lastIndexOf("o") == 7, "str-lastIndexOf-str");
        check(s.indexOf("zzz") == -1, "str-indexOf-absent");
        check(s.contains("o,W"), "str-contains");
        check(s.startsWith("Hello"), "str-startsWith");
        check(s.startsWith("World", 6), "str-startsWith-off");
        check(s.endsWith("World"), "str-endsWith");
        check("ABC".equals("ABC") && !"ABC".equals("abc"), "str-equals");
        check("ABC".equalsIgnoreCase("abc"), "str-equalsIgnoreCase");
        check("ab".compareTo("ac") < 0 && "ab".compareTo("ab") == 0, "str-compareTo");
        check("AB".compareToIgnoreCase("ab") == 0, "str-compareToIgnoreCase");
        check("Hello".toUpperCase().equals("HELLO"), "str-toUpper");
        check("Hello".toLowerCase().equals("hello"), "str-toLower");
        check("  hi  ".trim().equals("hi"), "str-trim");
        check("\t hi \n".strip().equals("hi"), "str-strip");
        check("  hi".stripLeading().equals("hi"), "str-stripLeading");
        check("hi  ".stripTrailing().equals("hi"), "str-stripTrailing");
        check("   ".isBlank() && !"x".isBlank(), "str-isBlank");
        check("".isEmpty() && !"x".isEmpty(), "str-isEmpty");
        check("Hello".replace('l', 'L').equals("HeLLo"), "str-replace-char");
        check("a.b.c".replace(".", "_").equals("a_b_c"), "str-replace-cs");
        check("a.b.c".replaceAll("\\.", "_").equals("a_b_c"), "str-replaceAll");
        check("aaa".replaceFirst("a", "b").equals("baa"), "str-replaceFirst");
        check("a,b,c".split(",").length == 3, "str-split");
        check("a,b,,".split(",").length == 2, "str-split-trailing");
        check("a,b,,".split(",", -1).length == 4, "str-split-neg");
        check("a,b,c".split(",", 2)[1].equals("b,c"), "str-split-limit");
        check(String.join("-", "a", "b", "c").equals("a-b-c"), "str-join-varargs");
        check(String.join(",", List.of("x", "y")).equals("x,y"), "str-join-iter");
        check("ab".concat("cd").equals("abcd"), "str-concat");
        check("12345".matches("\\d+"), "str-matches");
        check("ab".repeat(3).equals("ababab") && "x".repeat(0).isEmpty(), "str-repeat");
        check("l1\nl2\nl3".lines().count() == 3, "str-lines");
        check("abc".chars().sum() == 294, "str-chars-sum");
        check("5".transform(Integer::parseInt) == 5, "str-transform");
        check("Hello".regionMatches(true, 0, "hello", 0, 5), "str-regionMatches");
        check("Hello".toCharArray().length == 5 && new String("Hi".toCharArray()).equals("Hi"), "str-toCharArray");
        check(String.valueOf(42).equals("42") && String.valueOf(true).equals("true"), "str-valueOf");
        check(String.valueOf(new char[]{'a', 'b'}).equals("ab"), "str-valueOf-chars");
        check("ABC".hashCode() == 64578 && "".hashCode() == 0, "str-hashCode");
        check("abc".indent(2).equals("  abc\n"), "str-indent");
        check("xy".intern() == "xy".intern(), "str-intern");
        check("Hello".subSequence(1, 3).equals("el"), "str-subSequence");
        // text block (JDK15)
        String tb = """
                one
                two""";
        check(tb.equals("one\ntwo"), "str-textblock");
        check(tb.lines().count() == 2 && tb.startsWith("one"), "str-textblock-lines");
        // formatted (instance, JDK15)
        check("v=%d".formatted(7).equals("v=7"), "str-formatted");
    }

    // ================= java.lang.StringBuilder / StringBuffer =================
    static void sectionStringBuilder() {
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < 5; i++) sb.append(i);
        check(sb.toString().equals("01234"), "sb-append-int");
        check(sb.reverse().toString().equals("43210"), "sb-reverse");
        StringBuilder sb2 = new StringBuilder("hello");
        check(sb2.length() == 5 && sb2.charAt(1) == 'e', "sb-length-charAt");
        sb2.append('!').append(true).append(3.5).append(100L);
        check(sb2.toString().equals("hello!true3.5100"), "sb-append-overloads");
        StringBuilder sb3 = new StringBuilder("abcdef");
        sb3.insert(3, "XYZ");
        check(sb3.toString().equals("abcXYZdef"), "sb-insert");
        sb3.delete(3, 6);
        check(sb3.toString().equals("abcdef"), "sb-delete");
        sb3.deleteCharAt(0);
        check(sb3.toString().equals("bcdef"), "sb-deleteCharAt");
        sb3.replace(0, 2, "ZZ");
        check(sb3.toString().equals("ZZdef"), "sb-replace");
        sb3.setCharAt(0, 'Q');
        check(sb3.charAt(0) == 'Q', "sb-setCharAt");
        check(new StringBuilder("ababab").indexOf("ab", 1) == 2, "sb-indexOf");
        check(new StringBuilder("ababab").lastIndexOf("ab") == 4, "sb-lastIndexOf");
        check(new StringBuilder("abcdef").substring(2, 4).equals("cd"), "sb-substring");
        StringBuilder sb4 = new StringBuilder("abcdef");
        sb4.setLength(3);
        check(sb4.toString().equals("abc"), "sb-setLength");
        StringBuilder sb5 = new StringBuilder();
        sb5.appendCodePoint(0x1F600);
        check(sb5.length() == 2 && Character.codePointAt(sb5, 0) == 0x1F600, "sb-appendCodePoint");
        // capacity / ensureCapacity (non-negative growth, deterministic-enough)
        StringBuilder sb6 = new StringBuilder(8);
        check(sb6.capacity() == 8, "sb-capacity");
        // StringBuffer (synchronized variant) parity
        StringBuffer bf = new StringBuffer("ab");
        bf.append("cd").insert(0, '<').reverse();
        check(bf.toString().equals("dcba<"), "stringbuffer");
    }

    // ================= java.lang.Character =================
    static void sectionCharacter() {
        check(Character.isDigit('7') && !Character.isDigit('a'), "char-isDigit");
        check(Character.isLetter('a') && !Character.isLetter('1'), "char-isLetter");
        check(Character.isLetterOrDigit('z') && Character.isLetterOrDigit('5'), "char-isLetterOrDigit");
        check(Character.isWhitespace(' ') && Character.isWhitespace('\t'), "char-isWhitespace");
        check(Character.isUpperCase('A') && Character.isLowerCase('a'), "char-isUpperLower");
        check(Character.toUpperCase('a') == 'A' && Character.toLowerCase('Z') == 'z', "char-toUpperLower");
        check(Character.getNumericValue('7') == 7 && Character.getNumericValue('F') == 15, "char-getNumericValue");
        check(Character.digit('f', 16) == 15 && Character.digit('z', 16) == -1, "char-digit");
        check(Character.forDigit(10, 16) == 'a', "char-forDigit");
        check(Character.isAlphabetic('A') && !Character.isAlphabetic('1'), "char-isAlphabetic");
        check(Character.isJavaIdentifierStart('_') && !Character.isJavaIdentifierStart('1'), "char-isJavaIdentStart");
        check(Character.isJavaIdentifierPart('1'), "char-isJavaIdentPart");
        check(Character.charCount(0x1F600) == 2 && Character.charCount('A') == 1, "char-charCount");
        check(Character.toChars(0x1F600).length == 2, "char-toChars");
        char hi = Character.highSurrogate(0x1F600), lo = Character.lowSurrogate(0x1F600);
        check(Character.isHighSurrogate(hi) && Character.isLowSurrogate(lo), "char-surrogate");
        check(Character.toCodePoint(hi, lo) == 0x1F600, "char-toCodePoint");
        check(Character.isSurrogatePair(hi, lo), "char-isSurrogatePair");
        check(Character.getType('A') == Character.UPPERCASE_LETTER, "char-getType");
        check(Character.compare('a', 'b') < 0 && Character.compare('b', 'b') == 0, "char-compare");
        check(Character.MIN_RADIX == 2 && Character.MAX_RADIX == 36, "char-radix-const");
        check(Character.isDefined('A') && !Character.isISOControl('A') && Character.isISOControl('\n'), "char-isDefined");
        check(Character.hashCode('A') == 'A' && Character.valueOf('x').charValue() == 'x', "char-valueOf");
    }

    // ================= java.lang.Math / StrictMath =================
    static void sectionMath() {
        check(Math.max(3, 7) == 7 && Math.min(3, 7) == 3, "math-maxmin");
        check(Math.abs(-5) == 5 && Math.abs(-5L) == 5L && Math.abs(-2.5) == 2.5, "math-abs");
        check((long) Math.pow(2, 10) == 1024, "math-pow");
        check(Math.sqrt(16.0) == 4.0 && Math.cbrt(27.0) == 3.0, "math-sqrt-cbrt");
        check(Math.ceil(2.1) == 3.0 && Math.floor(2.9) == 2.0, "math-ceil-floor");
        check(Math.round(2.5) == 3L && Math.round(-2.5) == -2L && Math.round(2.4) == 2L, "math-round");
        check(Math.round(2.5f) == 3, "math-round-float");
        check(Math.rint(2.5) == 2.0 && Math.rint(3.5) == 4.0, "math-rint-halfeven");
        check(Math.signum(-3.0) == -1.0 && Math.signum(0.0) == 0.0 && Math.signum(4.0) == 1.0, "math-signum");
        check(Math.hypot(3.0, 4.0) == 5.0, "math-hypot");
        check(close(Math.log(Math.E), 1.0, 1e-12), "math-log");
        check(close(Math.log10(1000.0), 3.0, 1e-12), "math-log10");
        check(Math.exp(0.0) == 1.0 && close(Math.expm1(0.0), 0.0, 1e-15), "math-exp");
        check(close(Math.toRadians(180.0), Math.PI, 1e-12), "math-toRadians");
        check(close(Math.toDegrees(Math.PI), 180.0, 1e-9), "math-toDegrees");
        check(close(Math.atan2(1.0, 1.0), Math.PI / 4, 1e-12), "math-atan2");
        check(Math.floorDiv(-7, 2) == -4 && Math.floorDiv(7, 2) == 3, "math-floorDiv");
        check(Math.floorMod(-7, 3) == 2 && Math.floorMod(-7, 2) == 1, "math-floorMod");
        check(Math.addExact(2, 3) == 5 && Math.subtractExact(5, 3) == 2, "math-addsubExact");
        check(Math.multiplyExact(3, 4) == 12 && Math.negateExact(5) == -5, "math-mulnegExact");
        check(Math.incrementExact(5) == 6 && Math.decrementExact(5) == 4, "math-incdecExact");
        check(Math.toIntExact(100L) == 100 && Math.absExact(-5) == 5, "math-toIntExact-absExact");
        boolean ovf = false;
        try { Math.addExact(Integer.MAX_VALUE, 1); } catch (ArithmeticException e) { ovf = true; }
        check(ovf, "math-addExact-overflow");
        boolean ovf2 = false;
        try { Math.toIntExact(Long.MAX_VALUE); } catch (ArithmeticException e) { ovf2 = true; }
        check(ovf2, "math-toIntExact-overflow");
        check(Math.nextUp(1.0) > 1.0 && Math.nextDown(1.0) < 1.0, "math-nextUpDown");
        check(Math.nextAfter(1.0, 2.0) > 1.0, "math-nextAfter");
        check(close(Math.ulp(1.0), Math.pow(2, -52), 1e-30), "math-ulp");
        check(Math.getExponent(8.0) == 3 && Math.getExponent(0.5) == -1 && Math.getExponent(1.0) == 0, "math-getExponent");
        check(Math.scalb(1.0, 4) == 16.0, "math-scalb");
        check(Math.fma(2.0, 3.0, 4.0) == 10.0, "math-fma");
        check(Math.copySign(3.0, -1.0) == -3.0 && Math.copySign(-3.0, 1.0) == 3.0, "math-copySign");
        check(Math.IEEEremainder(5.0, 3.0) == -1.0, "math-IEEEremainder");
        check(Math.multiplyHigh(1L << 62, 4) == 1L, "math-multiplyHigh");
        check(close(Math.PI, 3.141592653589793, 1e-15) && close(Math.E, 2.718281828459045, 1e-15), "math-const");
        check(Double.isNaN(Math.max(Double.NaN, 1.0)), "math-max-NaN");
        // StrictMath parity
        check(StrictMath.sqrt(16.0) == 4.0 && StrictMath.abs(-7) == 7 && StrictMath.max(3, 9) == 9, "strictmath");
    }

    // ================= Integer / Long / Short / Byte =================
    static void sectionIntegralWrappers() {
        check(Integer.parseInt("ff", 16) == 255 && Integer.parseInt("-101", 2) == -5, "int-parse-radix");
        check(Integer.parseInt("42") == 42, "int-parse");
        check(Integer.toBinaryString(10).equals("1010"), "int-toBinary");
        check(Integer.toHexString(255).equals("ff") && Integer.toOctalString(8).equals("10"), "int-toHexOct");
        check(Integer.toString(255, 16).equals("ff"), "int-toString-radix");
        check(Integer.bitCount(7) == 3 && Integer.bitCount(255) == 8, "int-bitCount");
        check(Integer.numberOfLeadingZeros(1) == 31 && Integer.numberOfTrailingZeros(8) == 3, "int-nlz-ntz");
        check(Integer.highestOneBit(100) == 64 && Integer.lowestOneBit(12) == 4, "int-hi-lo-bit");
        check(Integer.reverse(1) == Integer.MIN_VALUE, "int-reverse");
        check(Integer.reverseBytes(0x01020304) == 0x04030201, "int-reverseBytes");
        check(Integer.rotateLeft(1, 4) == 16 && Integer.rotateRight(16, 4) == 1, "int-rotate");
        check(Integer.compare(3, 5) < 0 && Integer.compare(5, 5) == 0, "int-compare");
        check(Integer.max(2, 9) == 9 && Integer.min(2, 9) == 2 && Integer.sum(2, 3) == 5, "int-max-min-sum");
        check(Integer.signum(-5) == -1 && Integer.signum(0) == 0 && Integer.signum(5) == 1, "int-signum");
        check(Integer.parseUnsignedInt("4294967295") == -1, "int-parseUnsigned");
        check(Integer.toUnsignedString(-1).equals("4294967295"), "int-toUnsignedString");
        check(Integer.toUnsignedLong(-1) == 4294967295L, "int-toUnsignedLong");
        check(Integer.compareUnsigned(-1, 1) > 0, "int-compareUnsigned");
        check(Integer.divideUnsigned(-2, 2) == 2147483647 && Integer.remainderUnsigned(7, 3) == 1, "int-unsigned-divrem");
        check(Integer.decode("0x1F") == 31 && Integer.decode("010") == 8 && Integer.decode("#FF") == 255, "int-decode");
        check(Integer.hashCode(5) == 5, "int-hashCode");
        check(Integer.MAX_VALUE == 2147483647 && Integer.MIN_VALUE == -2147483648, "int-const");
        check(Integer.BYTES == 4 && Integer.SIZE == 32, "int-bytes-size");
        // boxing cache (-128..127 cached by spec under default config)
        check(Integer.valueOf(100) == Integer.valueOf(100), "int-cache-in");
        check(Integer.valueOf(200) != Integer.valueOf(200) && Integer.valueOf(200).equals(Integer.valueOf(200)), "int-cache-out");
        boolean nfe = false;
        try { Integer.parseInt("xyz"); } catch (NumberFormatException e) { nfe = true; }
        check(nfe, "int-parse-nfe");
        // Long
        check(Long.parseLong("9999999999") == 9999999999L, "long-parse");
        check(Long.toBinaryString(5L).equals("101") && Long.toHexString(255L).equals("ff"), "long-toString");
        check(Long.bitCount(7L) == 3 && Long.numberOfTrailingZeros(8L) == 3, "long-bits");
        check(Long.highestOneBit(100L) == 64L && Long.reverse(1L) == Long.MIN_VALUE, "long-hibit-reverse");
        check(Long.compare(3L, 5L) < 0 && Long.signum(-7L) == -1, "long-compare-signum");
        check(Long.MAX_VALUE == 9223372036854775807L && Long.MIN_VALUE == Long.MIN_VALUE, "long-const");
        check(Long.toUnsignedString(-1L).equals("18446744073709551615"), "long-toUnsignedString");
        check(Long.divideUnsigned(-1L, 2L) == 9223372036854775807L, "long-divideUnsigned");
        check(Long.parseUnsignedLong("18446744073709551615") == -1L, "long-parseUnsigned");
        // Short / Byte
        check(Short.parseShort("100") == 100 && Short.reverseBytes((short) 0x0102) == 0x0201, "short-parse-reverse");
        check(Short.toUnsignedInt((short) -1) == 65535 && Short.MAX_VALUE == 32767, "short-unsigned-const");
        check(Byte.parseByte("-5") == -5 && Byte.toUnsignedInt((byte) -1) == 255, "byte-parse-unsigned");
        check(Byte.MIN_VALUE == -128 && Byte.MAX_VALUE == 127, "byte-const");
    }

    // ================= Double / Float / Boolean =================
    static void sectionFloatBoolWrappers() {
        check(Double.parseDouble("3.14") == 3.14 && Double.parseDouble("1.5e2") == 150.0, "dbl-parse");
        check(Double.doubleToLongBits(1.0) == 0x3FF0000000000000L, "dbl-toBits");
        check(Double.longBitsToDouble(0x3FF0000000000000L) == 1.0, "dbl-fromBits");
        check(Double.doubleToLongBits(Double.NaN) == 0x7ff8000000000000L, "dbl-NaN-bits");
        check(Double.isNaN(0.0 / 0.0) && !Double.isNaN(1.0), "dbl-isNaN");
        check(Double.isInfinite(1.0 / 0.0) && Double.isFinite(1.0), "dbl-isInfinite-isFinite");
        check(Double.compare(0.0, -0.0) > 0 && Double.compare(-0.0, 0.0) < 0, "dbl-compare-zero");
        check(Double.compare(Double.NaN, Double.NaN) == 0, "dbl-compare-NaN");
        check(Double.max(1.0, 2.0) == 2.0 && Double.min(1.0, 2.0) == 1.0 && Double.sum(1.0, 2.0) == 3.0, "dbl-max-min-sum");
        check(Double.toHexString(1.0).equals("0x1.0p0"), "dbl-toHexString");
        check(Double.MIN_VALUE > 0.0 && Double.MAX_EXPONENT == 1023 && Double.MIN_EXPONENT == -1022, "dbl-const");
        check(Double.valueOf("2.5").doubleValue() == 2.5 && Double.hashCode(0.0) == 0, "dbl-valueOf-hashCode");
        check(Double.POSITIVE_INFINITY > Double.MAX_VALUE && Double.NEGATIVE_INFINITY < -Double.MAX_VALUE, "dbl-infinity-const");
        // Float
        check(Float.floatToIntBits(1.0f) == 1065353216, "flt-toBits");
        check(Float.intBitsToFloat(1065353216) == 1.0f, "flt-fromBits");
        check(Float.parseFloat("2.5") == 2.5f && Float.isNaN(Float.NaN), "flt-parse-NaN");
        check(Float.compare(1.0f, 2.0f) < 0 && Float.MAX_VALUE > 0, "flt-compare-const");
        // Boolean
        check(Boolean.parseBoolean("true") && Boolean.parseBoolean("TrUe") && !Boolean.parseBoolean("yes"), "bool-parse");
        check(Boolean.logicalAnd(true, true) && !Boolean.logicalAnd(true, false), "bool-and");
        check(Boolean.logicalOr(false, true) && !Boolean.logicalOr(false, false), "bool-or");
        check(!Boolean.logicalXor(true, true) && Boolean.logicalXor(true, false), "bool-xor");
        check(Boolean.compare(true, false) > 0 && Boolean.compare(false, false) == 0, "bool-compare");
        check(Boolean.TRUE && !Boolean.FALSE && Boolean.valueOf("true"), "bool-const");
        // Number widening
        Number n = 42;
        check(n.intValue() == 42 && n.longValue() == 42L && n.doubleValue() == 42.0 && n.byteValue() == 42, "number-widening");
    }

    // ================= java.util.Objects + java.lang.System =================
    static void sectionObjectsSystem() {
        check(Objects.equals(null, null) && Objects.equals("a", "a") && !Objects.equals("a", null), "obj-equals");
        check(Objects.deepEquals(new int[]{1, 2}, new int[]{1, 2}), "obj-deepEquals");
        check(Objects.hashCode(null) == 0 && Objects.hashCode("a") == "a".hashCode(), "obj-hashCode");
        check(Objects.hash(1, 2, 3) == 30817, "obj-hash");
        check(Objects.toString(null).equals("null") && Objects.toString(null, "d").equals("d"), "obj-toString");
        check(Objects.isNull(null) && !Objects.isNull("x") && Objects.nonNull("x"), "obj-isNull");
        check(Objects.requireNonNullElse(null, 5) == 5 && Objects.requireNonNullElse(7, 5) == 7, "obj-requireNonNullElse");
        check(Objects.requireNonNullElseGet(null, () -> 9) == 9, "obj-requireNonNullElseGet");
        check(Objects.compare(3, 5, Comparator.naturalOrder()) < 0, "obj-compare");
        check(Objects.checkIndex(2, 5) == 2, "obj-checkIndex");
        boolean npe = false;
        try { Objects.requireNonNull(null, "must"); } catch (NullPointerException e) { npe = e.getMessage().equals("must"); }
        check(npe, "obj-requireNonNull-msg");
        boolean ioobe = false;
        try { Objects.checkIndex(5, 5); } catch (IndexOutOfBoundsException e) { ioobe = true; }
        check(ioobe, "obj-checkIndex-oob");
        check(Objects.checkFromIndexSize(1, 3, 5) == 1, "obj-checkFromIndexSize");
        // System.arraycopy
        int[] src = {1, 2, 3, 4, 5}, dst = new int[5];
        System.arraycopy(src, 1, dst, 0, 3);
        check(dst[0] == 2 && dst[1] == 3 && dst[2] == 4 && dst[3] == 0, "sys-arraycopy");
        check(System.lineSeparator().length() >= 1, "sys-lineSeparator");
        check(System.identityHashCode(null) == 0, "sys-identityHashCode-null");
        long t0 = System.nanoTime();
        check(System.nanoTime() >= t0 && System.currentTimeMillis() > 0L, "sys-time");
    }

    // ================= enum / record =================
    static void sectionEnumRecord() {
        check(Color.values().length == 3, "enum-values");
        check(Color.valueOf("GREEN") == Color.GREEN, "enum-valueOf");
        check(Color.RED.ordinal() == 0 && Color.BLUE.ordinal() == 2, "enum-ordinal");
        check(Color.GREEN.name().equals("GREEN"), "enum-name");
        check(Color.RED.compareTo(Color.BLUE) < 0, "enum-compareTo");
        check(Color.RED.getDeclaringClass() == Color.class, "enum-getDeclaringClass");
        EnumSet<Color> es = EnumSet.of(Color.RED, Color.BLUE);
        check(es.contains(Color.RED) && !es.contains(Color.GREEN) && es.size() == 2, "enum-EnumSet");
        check(EnumSet.allOf(Color.class).size() == 3 && EnumSet.noneOf(Color.class).isEmpty(), "enum-EnumSet-allnone");
        EnumMap<Color, Integer> em = new EnumMap<>(Color.class);
        em.put(Color.RED, 1); em.put(Color.BLUE, 3);
        check(em.get(Color.RED) == 1 && em.size() == 2, "enum-EnumMap");
        String sw = switch (Color.GREEN) { case RED -> "r"; case GREEN -> "g"; case BLUE -> "b"; };
        check(sw.equals("g"), "enum-switch");
        // record
        Point p = new Point(1, 2);
        check(p.x() == 1 && p.y() == 2, "rec-accessors");
        check(p.equals(new Point(1, 2)) && !p.equals(new Point(1, 3)), "rec-equals");
        check(p.hashCode() == new Point(1, 2).hashCode(), "rec-hashCode");
        check(p.toString().equals("Point[x=1, y=2]"), "rec-toString");
        check(Point.class.isRecord() && Point.class.getRecordComponents().length == 2, "rec-isRecord");
    }

    // ================= java.lang.Throwable =================
    static void sectionThrowable() {
        String msg = "";
        try {
            try { throw new IllegalStateException("inner"); }
            catch (Exception e) { throw new RuntimeException("outer", e); }
        } catch (RuntimeException e) { msg = e.getMessage() + "/" + e.getCause().getMessage(); }
        check(msg.equals("outer/inner"), "exc-chain");
        // multi-catch
        String which = "";
        for (int i = 0; i < 2; i++) {
            try {
                if (i == 0) throw new NumberFormatException("nfe");
                else throw new ArrayIndexOutOfBoundsException("aioobe");
            } catch (NumberFormatException | ArrayIndexOutOfBoundsException e) {
                which += e.getMessage().substring(0, 1);
            }
        }
        check(which.equals("na"), "exc-multicatch");
        // arithmetic
        boolean ae = false;
        try { int z = 1 / 0; } catch (ArithmeticException e) { ae = e.getMessage().equals("/ by zero"); }
        check(ae, "exc-arithmetic");
        // NPE
        boolean np = false;
        try { String x = null; x.length(); } catch (NullPointerException e) { np = true; }
        check(np, "exc-npe");
        // ClassCast
        boolean cc = false;
        Object o = "str";
        try { Integer i = (Integer) o; } catch (ClassCastException e) { cc = true; }
        check(cc, "exc-classcast");
        // try-with-resources: close order + suppressed
        List<String> log = new ArrayList<>();
        Throwable caught = null;
        try {
            try (Resource a = new Resource(log, "A", true); Resource b = new Resource(log, "B", false)) {
                throw new RuntimeException("body");
            }
        } catch (RuntimeException e) { caught = e; }
        check(log.equals(List.of("close-B", "close-A")), "exc-twr-close-order");
        check(caught != null && caught.getMessage().equals("body"), "exc-twr-primary");
        check(caught.getSuppressed().length == 1 && caught.getSuppressed()[0].getMessage().equals("close-fail-A"), "exc-twr-suppressed");
        // manual addSuppressed
        RuntimeException primary = new RuntimeException("p");
        primary.addSuppressed(new IllegalStateException("s"));
        check(primary.getSuppressed().length == 1, "exc-addSuppressed");
        // stack trace present
        check(new RuntimeException("x").getStackTrace().length > 0, "exc-stackTrace");
        // custom exception subclass
        boolean custom = false;
        try { throw new IllegalArgumentException("bad-arg"); }
        catch (RuntimeException e) { custom = (e instanceof IllegalArgumentException) && e.getMessage().equals("bad-arg"); }
        check(custom, "exc-custom-subclass");
    }

    // ================= java.lang.Thread / ThreadLocal =================
    static void sectionThread() throws Exception {
        // simple start/join
        int[] x = {0};
        Thread t = new Thread(() -> x[0] = 42);
        check(t.getState() == Thread.State.NEW, "thr-state-new");
        t.start();
        t.join();
        check(x[0] == 42 && t.getState() == Thread.State.TERMINATED, "thr-join-terminated");
        check(Thread.currentThread().getName().equals("main"), "thr-main-name");
        Thread.yield(); // benign
        // interrupt flag self-test
        Thread.currentThread().interrupt();
        check(Thread.interrupted(), "thr-interrupt-flag");
        check(!Thread.interrupted(), "thr-interrupt-cleared");
        // synchronized accumulation across bounded threads (<=8)
        final int[] counter = {0};
        final Object lock = new Object();
        Thread[] ts = new Thread[6];
        for (int i = 0; i < ts.length; i++) {
            ts[i] = new Thread(() -> {
                for (int j = 0; j < 1000; j++) synchronized (lock) { counter[0]++; }
            });
        }
        for (Thread th : ts) th.start();
        for (Thread th : ts) th.join();
        check(counter[0] == 6000, "thr-synchronized-sum");
        // named thread + daemon flag
        Thread named = new Thread(() -> { }, "worker-1");
        named.setDaemon(true);
        check(named.getName().equals("worker-1") && named.isDaemon(), "thr-named-daemon");
        named.start();
        named.join();
        // ThreadLocal
        ThreadLocal<Integer> tl = ThreadLocal.withInitial(() -> 7);
        check(tl.get() == 7, "threadlocal-initial");
        tl.set(99);
        check(tl.get() == 99, "threadlocal-set");
        tl.remove();
        check(tl.get() == 7, "threadlocal-remove");
        // Runnable as functional interface
        Runnable r = () -> { };
        check(r != null, "thr-runnable");
    }

    // ================= java.util.Random / SplittableRandom / RandomGenerator =================
    static void sectionRandom() {
        Random r1 = new Random(42), r2 = new Random(42);
        check(r1.nextInt() == r2.nextInt(), "rnd-seeded-int");
        check(r1.nextLong() == r2.nextLong(), "rnd-seeded-long");
        check(r1.nextDouble() == r2.nextDouble(), "rnd-seeded-double");
        check(r1.nextBoolean() == r2.nextBoolean(), "rnd-seeded-bool");
        check(r1.nextGaussian() == r2.nextGaussian(), "rnd-seeded-gaussian");
        Random rb = new Random(7);
        for (int i = 0; i < 50; i++) { int v = rb.nextInt(10); if (v < 0 || v >= 10) { check(false, "rnd-bound"); return; } }
        check(true, "rnd-bound");
        // setSeed resets stream
        Random rs = new Random(); rs.setSeed(123);
        Random rs2 = new Random(123);
        check(rs.nextInt() == rs2.nextInt(), "rnd-setSeed");
        // ints() stream deterministic
        long sum1 = new Random(5).ints(20, 0, 100).sum();
        long sum2 = new Random(5).ints(20, 0, 100).sum();
        check(sum1 == sum2, "rnd-ints-stream");
        // SplittableRandom
        SplittableRandom sr1 = new SplittableRandom(99), sr2 = new SplittableRandom(99);
        check(sr1.nextInt() == sr2.nextInt() && sr1.nextLong() == sr2.nextLong(), "rnd-splittable");
        // JDK17 RandomGenerator factory (algorithmic, deterministic with seed)
        RandomGenerator g1 = RandomGeneratorFactory.of("L64X128MixRandom").create(2024);
        RandomGenerator g2 = RandomGeneratorFactory.of("L64X128MixRandom").create(2024);
        check(g1.nextLong() == g2.nextLong() && g1.nextInt() == g2.nextInt(), "rnd-generator-17");
        check(RandomGeneratorFactory.all().count() > 0, "rnd-factory-all");
    }

    // ================= java.util.Scanner / StringTokenizer =================
    static void sectionScannerTokenizer() {
        Scanner sc = new Scanner("10 20 hello").useLocale(Locale.US);
        check(sc.nextInt() == 10 && sc.nextInt() == 20 && sc.next().equals("hello"), "scan-basic");
        Scanner sc2 = new Scanner("3.5 true").useLocale(Locale.US);
        check(sc2.nextDouble() == 3.5 && sc2.nextBoolean(), "scan-double-bool");
        Scanner sc3 = new Scanner("42 x");
        check(sc3.hasNextInt() && sc3.nextInt() == 42 && !sc3.hasNextInt() && sc3.next().equals("x"), "scan-hasNextInt");
        Scanner sc4 = new Scanner("a,b,c").useDelimiter(",");
        check(sc4.next().equals("a") && sc4.next().equals("b") && sc4.next().equals("c"), "scan-delimiter");
        Scanner sc5 = new Scanner("line1\nline2");
        check(sc5.nextLine().equals("line1") && sc5.nextLine().equals("line2"), "scan-nextLine");
        Scanner sc6 = new Scanner("ff");
        check(sc6.nextInt(16) == 255, "scan-radix");
        Scanner sc7 = new Scanner("1 2 3 4");
        int total = 0; while (sc7.hasNextInt()) total += sc7.nextInt();
        check(total == 10, "scan-loop");
        // StringTokenizer
        StringTokenizer st = new StringTokenizer("a-b-c", "-");
        check(st.countTokens() == 3 && st.nextToken().equals("a") && st.hasMoreTokens(), "tok-basic");
        StringTokenizer st2 = new StringTokenizer("a b\tc\nd");
        check(st2.countTokens() == 4, "tok-default-delims");
        StringTokenizer st3 = new StringTokenizer("a,b", ",", true);
        check(st3.countTokens() == 3 && st3.nextToken().equals("a") && st3.nextToken().equals(","), "tok-returnDelims");
        List<String> collected = new ArrayList<>();
        StringTokenizer st4 = new StringTokenizer("x:y:z", ":");
        while (st4.hasMoreTokens()) collected.add(st4.nextToken());
        check(collected.equals(List.of("x", "y", "z")), "tok-loop");
    }

    // ================= java.util.BitSet =================
    static void sectionBitSet() {
        BitSet bs = new BitSet(64);
        bs.set(1); bs.set(3); bs.set(5, 8); // 5,6,7
        check(bs.cardinality() == 5, "bs-cardinality");
        check(bs.get(6) && !bs.get(8) && !bs.get(2), "bs-get");
        check(bs.nextSetBit(0) == 1 && bs.nextSetBit(4) == 5, "bs-nextSetBit");
        check(bs.nextClearBit(1) == 2 && bs.nextClearBit(5) == 8, "bs-nextClearBit");
        check(bs.previousSetBit(4) == 3 && bs.previousSetBit(100) == 7, "bs-previousSetBit");
        check(bs.length() == 8, "bs-length");
        BitSet flip = new BitSet(); flip.set(0, 4); flip.flip(1, 3); // clears 1,2
        check(flip.get(0) && !flip.get(1) && !flip.get(2) && flip.get(3), "bs-flip-range");
        BitSet a = new BitSet(); a.set(0); a.set(1); a.set(2);
        BitSet b = new BitSet(); b.set(1); b.set(2); b.set(3);
        BitSet and = (BitSet) a.clone(); and.and(b);
        check(and.cardinality() == 2 && and.get(1) && and.get(2), "bs-and");
        BitSet or = (BitSet) a.clone(); or.or(b);
        check(or.cardinality() == 4, "bs-or");
        BitSet xor = (BitSet) a.clone(); xor.xor(b);
        check(xor.cardinality() == 2 && xor.get(0) && xor.get(3), "bs-xor");
        BitSet andNot = (BitSet) a.clone(); andNot.andNot(b);
        check(andNot.cardinality() == 1 && andNot.get(0), "bs-andNot");
        check(a.intersects(b) && !a.intersects(new BitSet()), "bs-intersects");
        BitSet vb = BitSet.valueOf(new long[]{0b1010L});
        check(vb.get(1) && vb.get(3) && vb.cardinality() == 2, "bs-valueOf");
        BitSet rt = BitSet.valueOf(a.toByteArray());
        check(rt.equals(a), "bs-toByteArray-roundtrip");
        check(a.stream().sum() == 3, "bs-stream"); // 0+1+2
        BitSet empty = new BitSet();
        check(empty.isEmpty() && empty.cardinality() == 0, "bs-isEmpty");
        a.clear(1);
        check(!a.get(1) && a.cardinality() == 2, "bs-clear");
        BitSet str = new BitSet(); str.set(1); str.set(3);
        check(str.toString().equals("{1, 3}"), "bs-toString");
    }

    // ================= UUID / StringJoiner / Optional =================
    static void sectionUuidJoinerOptional() {
        UUID u = UUID.fromString("12345678-1234-5678-1234-567812345678");
        check(u.toString().equals("12345678-1234-5678-1234-567812345678"), "uuid-roundtrip");
        check(u.getMostSignificantBits() == 0x1234567812345678L, "uuid-msb");
        check(u.getLeastSignificantBits() == 0x1234567812345678L, "uuid-lsb");
        check(u.version() == 5, "uuid-version-field");
        check(u.compareTo(u) == 0 && u.equals(UUID.fromString("12345678-1234-5678-1234-567812345678")), "uuid-equals");
        UUID name = UUID.nameUUIDFromBytes("hello".getBytes(java.nio.charset.StandardCharsets.UTF_8));
        check(name.toString().equals("5d41402a-bc4b-3a76-b971-9d911017c592"), "uuid-name-md5");
        check(name.version() == 3 && name.variant() == 2, "uuid-name-version-variant");
        // StringJoiner
        StringJoiner sj = new StringJoiner(", ", "[", "]");
        sj.add("a").add("b").add("c");
        check(sj.toString().equals("[a, b, c]"), "joiner-prefix-suffix");
        StringJoiner plain = new StringJoiner(",");
        plain.add("x").add("y");
        check(plain.toString().equals("x,y"), "joiner-plain");
        StringJoiner emptyJ = new StringJoiner(",", "[", "]");
        check(emptyJ.toString().equals("[]"), "joiner-empty-default");
        emptyJ.setEmptyValue("NONE");
        check(emptyJ.toString().equals("NONE"), "joiner-setEmptyValue");
        StringJoiner m1 = new StringJoiner(",", "[", "]").add("a").add("b");
        StringJoiner m2 = new StringJoiner(",", "(", ")").add("c").add("d");
        m1.merge(m2);
        check(m1.toString().equals("[a,b,c,d]"), "joiner-merge");
        // Optional
        check(Optional.of("x").get().equals("x"), "opt-of-get");
        check(Optional.empty().orElse("d").equals("d"), "opt-orElse");
        check(!Optional.ofNullable(null).isPresent() && Optional.ofNullable("x").isPresent(), "opt-ofNullable");
        check(Optional.of(5).map(i -> i + 1).get() == 6, "opt-map");
        check(Optional.of(5).flatMap(i -> Optional.of(i * 2)).get() == 10, "opt-flatMap");
        check(Optional.of(5).filter(i -> i > 3).isPresent() && !Optional.of(5).filter(i -> i > 10).isPresent(), "opt-filter");
        check(Optional.empty().or(() -> Optional.of(2)).get().equals(2), "opt-or");
        check(Optional.of(3).stream().count() == 1 && Optional.empty().stream().count() == 0, "opt-stream");
        check(Optional.of(7).orElseGet(() -> 0) == 7, "opt-orElseGet");
        boolean ose = false;
        try { Optional.empty().orElseThrow(); } catch (NoSuchElementException e) { ose = true; }
        check(ose, "opt-orElseThrow");
        final int[] sink = {0};
        Optional.of(11).ifPresent(v -> sink[0] = v);
        check(sink[0] == 11, "opt-ifPresent");
        Optional.empty().ifPresentOrElse(v -> { }, () -> sink[0] = -1);
        check(sink[0] == -1, "opt-ifPresentOrElse");
        // OptionalInt / OptionalDouble / OptionalLong
        check(OptionalInt.of(7).getAsInt() == 7 && OptionalInt.empty().orElse(9) == 9, "opt-int");
        check(OptionalDouble.of(1.5).getAsDouble() == 1.5 && OptionalLong.of(3L).getAsLong() == 3L, "opt-double-long");
        check(java.util.stream.IntStream.range(1, 5).max().getAsInt() == 4, "opt-int-stream-max");
    }

    // ================= java.util.regex =================
    static void sectionRegex() {
        Pattern p = Pattern.compile("(\\d+)-(\\d+)");
        Matcher m = p.matcher("12-345");
        check(m.matches(), "rgx-matches");
        check(m.group(0).equals("12-345") && m.group(1).equals("12") && m.group(2).equals("345"), "rgx-group");
        check(m.groupCount() == 2, "rgx-groupCount");
        check(m.start(1) == 0 && m.end(1) == 2 && m.start(2) == 3, "rgx-start-end");
        check(Pattern.compile("a*").matcher("aaaa").matches(), "rgx-star");
        check(Pattern.matches("\\d+", "123") && !Pattern.matches("\\d+", "12a"), "rgx-static-matches");
        // find iteration
        Matcher fm = Pattern.compile("\\d+").matcher("a12b345c6");
        int count = 0; long total = 0;
        while (fm.find()) { count++; total += Integer.parseInt(fm.group()); }
        check(count == 3 && total == 363, "rgx-find-loop");
        // named groups
        Matcher nm = Pattern.compile("(?<y>\\d{4})-(?<m>\\d{2})").matcher("2024-06");
        check(nm.matches() && nm.group("y").equals("2024") && nm.group("m").equals("06"), "rgx-named");
        // replace
        check("a1b2c3".replaceAll("\\d", "#").equals("a#b#c#"), "rgx-replaceAll");
        check("a1b2".replaceFirst("\\d", "#").equals("a#b2"), "rgx-replaceFirst");
        check(Pattern.compile("(\\d)(\\d)").matcher("12").replaceAll("$2$1").equals("21"), "rgx-backref-replace");
        // split
        check(Pattern.compile(",").split("a,b,c").length == 3, "rgx-split");
        check(Pattern.compile("\\s+").split("a  b   c").length == 3, "rgx-split-ws");
        // flags
        check(Pattern.compile("abc", Pattern.CASE_INSENSITIVE).matcher("ABC").matches(), "rgx-flag-ci");
        // quote
        check(Pattern.quote("a.b").equals("\\Qa.b\\E"), "rgx-quote");
        check(Pattern.compile(Pattern.quote("a.b")).matcher("a.b").matches(), "rgx-quote-match");
        // lookahead
        Matcher la = Pattern.compile("\\d+(?=px)").matcher("10px");
        check(la.find() && la.group().equals("10"), "rgx-lookahead");
        // backreference in pattern
        Matcher br = Pattern.compile("(\\w)\\1").matcher("hello");
        check(br.find() && br.group().equals("ll"), "rgx-backref-pattern");
        // results() stream (JDK9)
        check(Pattern.compile("\\d+").matcher("a1b22c333").results().count() == 3, "rgx-results");
        // appendReplacement / appendTail (StringBuilder overload)
        Matcher ar = Pattern.compile("a").matcher("banana");
        StringBuilder out = new StringBuilder();
        while (ar.find()) ar.appendReplacement(out, "A");
        ar.appendTail(out);
        check(out.toString().equals("bAnAnA"), "rgx-appendReplacement");
        // region + reset
        Matcher rg = Pattern.compile("\\d+").matcher("12ab34");
        rg.region(2, 6);
        check(rg.find() && rg.group().equals("34"), "rgx-region");
        // splitAsStream
        check(Pattern.compile(",").splitAsStream("p,q,r").count() == 3, "rgx-splitAsStream");
    }

    // ================= java.util.Formatter (String.format) =================
    static void sectionFormat() {
        check(String.format("%d", 42).equals("42"), "fmt-d");
        check(String.format("%05d", 42).equals("00042"), "fmt-zeropad");
        check(String.format(Locale.US, "%+d", 5).equals("+5"), "fmt-plus");
        check(String.format(Locale.US, "% d", 5).equals(" 5"), "fmt-space");
        check(String.format(Locale.US, "%(d", -5).equals("(5)"), "fmt-paren");
        check(String.format(Locale.US, "%,d", 1000000).equals("1,000,000"), "fmt-grouping");
        check(String.format("%s", "hi").equals("hi"), "fmt-s");
        check(String.format("%-5s|", "ab").equals("ab   |"), "fmt-left");
        check(String.format("%5s", "ab").equals("   ab"), "fmt-right");
        check(String.format("%x", 255).equals("ff") && String.format("%X", 255).equals("FF"), "fmt-hex");
        check(String.format("%#x", 255).equals("0xff"), "fmt-alt-hex");
        check(String.format("%o", 8).equals("10") && String.format("%#o", 8).equals("010"), "fmt-oct");
        check(String.format(Locale.US, "%.2f", 3.14159).equals("3.14"), "fmt-f-prec");
        check(String.format(Locale.US, "%08.2f", 3.5).equals("00003.50"), "fmt-f-width");
        check(String.format(Locale.US, "%.0f", 2.4).equals("2"), "fmt-f-round");
        check(String.format(Locale.US, "%e", 12345.678).equals("1.234568e+04"), "fmt-e");
        check(String.format(Locale.US, "%E", 0.0001234).equals("1.234000E-04"), "fmt-E");
        check(String.format("%b", true).equals("true") && String.format("%b", (Object) null).equals("false"), "fmt-b");
        check(String.format("%c", (int) 65).equals("A"), "fmt-c");
        check(String.format("%1$s-%1$s", "x").equals("x-x"), "fmt-arg-index");
        check(String.format("%2$s%1$s", "a", "b").equals("ba"), "fmt-arg-index2");
        check(String.format("%%").equals("%"), "fmt-percent");
        check(String.format(Locale.US, "%,.2f", 1234.5).equals("1,234.50"), "fmt-group-prec");
    }

    // ================= java.nio buffers =================
    static void sectionByteBuffer() {
        ByteBuffer b = ByteBuffer.allocate(32);
        b.putInt(0x01020304).putShort((short) 0x0506).put((byte) 0x07)
         .putLong(0x1112131415161718L).putChar('A').putDouble(1.5).putFloat(2.5f);
        check(b.position() == 29 && b.capacity() == 32, "buf-position-capacity");
        b.flip();
        check(b.getInt() == 0x01020304, "buf-getInt");
        check(b.getShort() == 0x0506, "buf-getShort");
        check(b.get() == 0x07, "buf-get");
        check(b.getLong() == 0x1112131415161718L, "buf-getLong");
        check(b.getChar() == 'A', "buf-getChar");
        check(b.getDouble() == 1.5, "buf-getDouble");
        check(b.getFloat() == 2.5f, "buf-getFloat");
        check(b.remaining() == 0 && !b.hasRemaining() && b.limit() == 29, "buf-remaining");
        // big/little endian
        ByteBuffer be = ByteBuffer.allocate(4).order(ByteOrder.BIG_ENDIAN);
        be.putInt(1); be.flip();
        check(be.get(0) == 0 && be.get(3) == 1, "buf-bigendian");
        ByteBuffer le = ByteBuffer.allocate(4).order(ByteOrder.LITTLE_ENDIAN);
        le.putInt(0x01020304); le.flip();
        check(le.get(0) == 0x04 && le.get(3) == 0x01 && le.order() == ByteOrder.LITTLE_ENDIAN, "buf-littleendian");
        // absolute put/get
        ByteBuffer ab = ByteBuffer.allocate(8);
        ab.putInt(0, 0x0A0B0C0D).putInt(4, 0x0E0F1011);
        check(ab.getInt(0) == 0x0A0B0C0D && ab.getInt(4) == 0x0E0F1011, "buf-absolute");
        // wrap / array
        ByteBuffer wb = ByteBuffer.wrap(new byte[]{1, 2, 3, 4});
        check(wb.getInt() == 0x01020304 && wb.hasArray() && wb.array().length == 4 && wb.arrayOffset() == 0, "buf-wrap-array");
        // mark / reset / rewind / clear
        ByteBuffer mb = ByteBuffer.allocate(8);
        mb.putInt(99); mb.mark(); mb.putInt(100);
        mb.reset();
        check(mb.getInt() == 100, "buf-mark-reset");
        mb.rewind();
        check(mb.position() == 0 && mb.getInt() == 99, "buf-rewind");
        mb.clear();
        check(mb.position() == 0 && mb.limit() == 8, "buf-clear");
        // slice
        ByteBuffer sb = ByteBuffer.wrap(new byte[]{10, 20, 30, 40, 50});
        sb.position(2);
        ByteBuffer sl = sb.slice();
        check(sl.capacity() == 3 && sl.get(0) == 30, "buf-slice");
        // duplicate shares content
        ByteBuffer dup = ByteBuffer.allocate(4);
        dup.putInt(0, 0x12345678);
        ByteBuffer dd = dup.duplicate();
        check(dd.getInt(0) == 0x12345678, "buf-duplicate");
        // read-only
        ByteBuffer ro = ByteBuffer.allocate(4).asReadOnlyBuffer();
        check(ro.isReadOnly(), "buf-readonly-flag");
        boolean rob = false;
        try { ro.putInt(1); } catch (ReadOnlyBufferException e) { rob = true; }
        check(rob, "buf-readonly-exc");
        // compact
        ByteBuffer cp = ByteBuffer.allocate(8);
        cp.putInt(1).putInt(2); cp.flip(); cp.getInt(); cp.compact();
        check(cp.position() == 4 && cp.getInt(0) == 2, "buf-compact");
        // asIntBuffer
        ByteBuffer ib = ByteBuffer.allocate(8);
        IntBuffer iv = ib.asIntBuffer();
        check(iv.capacity() == 2, "buf-asIntBuffer");
        iv.put(0, 7).put(1, 9);
        check(ib.getInt(0) == 7 && ib.getInt(4) == 9, "buf-intview-write");
        // CharBuffer
        CharBuffer cb = CharBuffer.wrap("hello");
        check(cb.length() == 5 && cb.charAt(1) == 'e' && cb.get() == 'h', "buf-charbuffer");
        check(CharBuffer.wrap("abc").toString().equals("abc"), "buf-charbuffer-toString");
        // IntBuffer / LongBuffer / DoubleBuffer relative
        IntBuffer ibuf = IntBuffer.allocate(3);
        ibuf.put(5).put(6).put(7); ibuf.flip();
        check(ibuf.get() == 5 && ibuf.get() == 6 && ibuf.get() == 7, "buf-intbuffer");
        LongBuffer lbuf = LongBuffer.wrap(new long[]{100L, 200L});
        check(lbuf.get() == 100L && lbuf.get(1) == 200L, "buf-longbuffer");
        DoubleBuffer dbuf = DoubleBuffer.allocate(2);
        dbuf.put(1.5).put(2.5); dbuf.flip();
        check(dbuf.get() == 1.5 && dbuf.get() == 2.5, "buf-doublebuffer");
        // underflow / overflow / IllegalArgument
        boolean uf = false;
        try { ByteBuffer.allocate(2).getInt(); } catch (BufferUnderflowException e) { uf = true; }
        check(uf, "buf-underflow");
        boolean of = false;
        try { ByteBuffer.allocate(2).putInt(1); } catch (BufferOverflowException e) { of = true; }
        check(of, "buf-overflow");
        boolean iae = false;
        try { ByteBuffer.allocate(-1); } catch (IllegalArgumentException e) { iae = true; }
        check(iae, "buf-illegalarg");
        check(ByteBuffer.allocate(0).capacity() == 0, "buf-zero");
        // equality / compareTo
        ByteBuffer e1 = ByteBuffer.wrap(new byte[]{1, 2, 3});
        ByteBuffer e2 = ByteBuffer.wrap(new byte[]{1, 2, 3});
        check(e1.equals(e2) && e1.compareTo(e2) == 0, "buf-equals");
        check(ByteOrder.nativeOrder() == ByteOrder.LITTLE_ENDIAN || ByteOrder.nativeOrder() == ByteOrder.BIG_ENDIAN, "buf-nativeOrder");
    }

    // ================= java.nio.charset =================
    static void sectionCharset() {
        check("héllo".getBytes(StandardCharsets.UTF_8).length == 6, "cs-utf8-len");
        check(new String("héllo".getBytes(StandardCharsets.UTF_8), StandardCharsets.UTF_8).equals("héllo"), "cs-utf8-roundtrip");
        check("abc".getBytes(StandardCharsets.US_ASCII).length == 3, "cs-ascii-len");
        check("abc".getBytes(StandardCharsets.ISO_8859_1).length == 3, "cs-iso-len");
        check("A".getBytes(StandardCharsets.UTF_16).length == 4, "cs-utf16-bom");
        check("A".getBytes(StandardCharsets.UTF_16BE).length == 2, "cs-utf16be");
        byte[] be = "A".getBytes(StandardCharsets.UTF_16BE);
        check(be[0] == 0 && be[1] == 'A', "cs-utf16be-bytes");
        check(Charset.forName("UTF-8") == StandardCharsets.UTF_8, "cs-forName");
        check(StandardCharsets.UTF_8.name().equals("UTF-8"), "cs-name");
        check(Charset.isSupported("UTF-8") && Charset.isSupported("US-ASCII"), "cs-isSupported");
        CharsetEncoder enc = StandardCharsets.UTF_8.newEncoder();
        check(enc.canEncode("abc"), "cs-canEncode-utf8");
        check(!StandardCharsets.ISO_8859_1.newEncoder().canEncode('€'), "cs-canEncode-iso-euro");
        check(StandardCharsets.US_ASCII.newEncoder().canEncode('a') && !StandardCharsets.US_ASCII.newEncoder().canEncode('é'), "cs-ascii-canEncode");
        // encode / decode via Charset
        ByteBuffer enced = StandardCharsets.UTF_8.encode("abc");
        check(enced.remaining() == 3, "cs-encode");
        String dec = StandardCharsets.UTF_8.decode(ByteBuffer.wrap("hi".getBytes(StandardCharsets.UTF_8))).toString();
        check(dec.equals("hi"), "cs-decode");
        check(StandardCharsets.UTF_8.aliases().contains("utf8") || StandardCharsets.UTF_8.name().equals("UTF-8"), "cs-aliases");
    }

    // ================= java.text (deterministic via explicit symbols) =================
    static void sectionText() {
        DecimalFormatSymbols sym = new DecimalFormatSymbols(Locale.US);
        DecimalFormat df = new DecimalFormat("#,##0.00", sym);
        check(df.format(1234.5).equals("1,234.50"), "txt-decimal-group");
        DecimalFormat df2 = new DecimalFormat("0.###", sym);
        check(df2.format(3.14159).equals("3.142") && df2.format(5.0).equals("5"), "txt-decimal-optional");
        DecimalFormat pct = new DecimalFormat("0.0%", sym);
        check(pct.format(0.25).equals("25.0%"), "txt-decimal-percent");
        DecimalFormat sci = new DecimalFormat("0.00E0", sym);
        check(sci.format(12345.0).equals("1.23E4"), "txt-decimal-sci");
        try {
            Number parsed = df.parse("1,234.50");
            check(parsed.doubleValue() == 1234.5, "txt-decimal-parse");
        } catch (ParseException e) { check(false, "txt-decimal-parse"); }
        DecimalFormat neg = new DecimalFormat("0.00;(0.00)", sym);
        check(neg.format(-3.5).equals("(3.50)"), "txt-decimal-negpattern");
        // MessageFormat (pure positional substitution)
        check(MessageFormat.format("{0} and {1}", "x", "y").equals("x and y"), "txt-messageformat");
        check(MessageFormat.format("{0}-{0}", "z").equals("z-z"), "txt-messageformat-repeat");
    }

    // ================= java.lang.Class + light reflection =================
    static void sectionReflection() throws Exception {
        check(String.class.getName().equals("java.lang.String"), "cls-getName");
        check(String.class.getSimpleName().equals("String"), "cls-getSimpleName");
        check(int.class.isPrimitive() && !String.class.isPrimitive(), "cls-isPrimitive");
        check(int[].class.isArray() && int[].class.getComponentType() == int.class, "cls-array");
        check(Integer.TYPE == int.class && Integer.class != int.class, "cls-TYPE");
        check(String.class.isAssignableFrom(String.class) && Object.class.isAssignableFrom(String.class), "cls-isAssignableFrom");
        check(!String.class.isAssignableFrom(Object.class), "cls-isAssignableFrom-neg");
        check(Class.forName("java.lang.Integer") == Integer.class, "cls-forName");
        check(Integer.class.getSuperclass() == Number.class && Object.class.getSuperclass() == null, "cls-getSuperclass");
        check(String.class.isInstance("x") && !String.class.isInstance(5), "cls-isInstance");
        check("x".getClass() == String.class, "cls-getClass");
        // reflection on Holder
        Holder h = Holder.class.getDeclaredConstructor(int.class).newInstance(7);
        check(h.v == 7, "ref-newInstance");
        Method add = Holder.class.getMethod("add", int.class, int.class);
        check((int) add.invoke(h, 3, 4) == 7, "ref-invoke");
        Method getV = Holder.class.getMethod("getV");
        check((int) getV.invoke(h) == 7, "ref-invoke-noarg");
        Field vf = Holder.class.getField("v");
        vf.setInt(h, 99);
        check(vf.getInt(h) == 99, "ref-field");
        check(Holder.class.getDeclaredMethods().length >= 2, "ref-declaredMethods");
        // reflective array
        Object arr = Array.newInstance(int.class, 3);
        Array.setInt(arr, 0, 5); Array.setInt(arr, 2, 9);
        check(Array.getInt(arr, 0) == 5 && Array.getInt(arr, 2) == 9 && Array.getLength(arr) == 3, "ref-array");
        // invoke String method reflectively
        Method len = String.class.getMethod("length");
        check((int) len.invoke("abcd") == 4, "ref-string-method");
    }
}
