package org.starry.dod;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collections;
import java.util.Date;
import java.util.HashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.TimeZone;
import java.util.Calendar;
import java.util.function.Predicate;

import org.apache.commons.lang3.ArrayUtils;
import org.apache.commons.lang3.BooleanUtils;
import org.apache.commons.lang3.CharSetUtils;
import org.apache.commons.lang3.CharUtils;
import org.apache.commons.lang3.ClassUtils;
import org.apache.commons.lang3.EnumUtils;
import org.apache.commons.lang3.ObjectUtils;
import org.apache.commons.lang3.RandomStringUtils;
import org.apache.commons.lang3.RandomUtils;
import org.apache.commons.lang3.Range;
import org.apache.commons.lang3.RegExUtils;
import org.apache.commons.lang3.StringEscapeUtils;
import org.apache.commons.lang3.StringUtils;
import org.apache.commons.lang3.SystemUtils;
import org.apache.commons.lang3.JavaVersion;
import org.apache.commons.lang3.Validate;
import org.apache.commons.lang3.builder.CompareToBuilder;
import org.apache.commons.lang3.builder.EqualsBuilder;
import org.apache.commons.lang3.builder.HashCodeBuilder;
import org.apache.commons.lang3.builder.ReflectionToStringBuilder;
import org.apache.commons.lang3.builder.ToStringBuilder;
import org.apache.commons.lang3.builder.ToStringStyle;
import org.apache.commons.lang3.compare.ComparableUtils;
import org.apache.commons.lang3.exception.ExceptionUtils;
import org.apache.commons.lang3.math.Fraction;
import org.apache.commons.lang3.math.NumberUtils;
import org.apache.commons.lang3.mutable.MutableInt;
import org.apache.commons.lang3.mutable.MutableObject;
import org.apache.commons.lang3.reflect.FieldUtils;
import org.apache.commons.lang3.reflect.MethodUtils;
import org.apache.commons.lang3.text.StrBuilder;
import org.apache.commons.lang3.text.StrSubstitutor;
import org.apache.commons.lang3.text.StrTokenizer;
import org.apache.commons.lang3.text.WordUtils;
import org.apache.commons.lang3.time.DateFormatUtils;
import org.apache.commons.lang3.time.DateUtils;
import org.apache.commons.lang3.time.DurationFormatUtils;
import org.apache.commons.lang3.time.StopWatch;
import org.apache.commons.lang3.tuple.ImmutablePair;
import org.apache.commons.lang3.tuple.MutablePair;
import org.apache.commons.lang3.tuple.Pair;
import org.apache.commons.lang3.tuple.Triple;

/**
 * Carpet-level coverage for the commons-lang3 (3.14.0) third-party library.
 * Deterministic and offline: all inputs are self-fabricated and assertions
 * check exact values (equals / == / fixed strings), not "ran without error".
 */
public class Lang3Carpet {

    private static int ok = 0;
    private static int fail = 0;

    private static void tru(String name, boolean cond) {
        if (cond) {
            ok++;
        } else {
            fail++;
            System.out.println("FAIL " + name);
        }
    }

    private static void fls(String name, boolean cond) {
        tru(name, !cond);
    }

    private static void eq(String name, Object expected, Object actual) {
        boolean good = (expected == null) ? actual == null : expected.equals(actual);
        if (good) {
            ok++;
        } else {
            fail++;
            System.out.println("FAIL " + name + " expected=[" + expected + "] actual=[" + actual + "]");
        }
    }

    private static void eqi(String name, int expected, int actual) {
        if (expected == actual) {
            ok++;
        } else {
            fail++;
            System.out.println("FAIL " + name + " expected=" + expected + " actual=" + actual);
        }
    }

    private static void eql(String name, long expected, long actual) {
        if (expected == actual) {
            ok++;
        } else {
            fail++;
            System.out.println("FAIL " + name + " expected=" + expected + " actual=" + actual);
        }
    }

    private static void eqd(String name, double expected, double actual, double eps) {
        if (Math.abs(expected - actual) <= eps) {
            ok++;
        } else {
            fail++;
            System.out.println("FAIL " + name + " expected=" + expected + " actual=" + actual);
        }
    }

    private static void eqArr(String name, int[] expected, int[] actual) {
        if (Arrays.equals(expected, actual)) {
            ok++;
        } else {
            fail++;
            System.out.println("FAIL " + name + " expected=" + Arrays.toString(expected)
                    + " actual=" + Arrays.toString(actual));
        }
    }

    private static void eqArr(String name, Object[] expected, Object[] actual) {
        if (Arrays.equals(expected, actual)) {
            ok++;
        } else {
            fail++;
            System.out.println("FAIL " + name + " expected=" + Arrays.toString(expected)
                    + " actual=" + Arrays.toString(actual));
        }
    }

    private static void throwsType(String name, Class<? extends Throwable> type, Runnable r) {
        try {
            r.run();
            fail++;
            System.out.println("FAIL " + name + " expected " + type.getSimpleName() + " but nothing thrown");
        } catch (Throwable t) {
            if (type.isInstance(t)) {
                ok++;
            } else {
                fail++;
                System.out.println("FAIL " + name + " expected " + type.getSimpleName()
                        + " got " + t.getClass().getSimpleName());
            }
        }
    }

    private static boolean allMatch(String s, Predicate<Character> p) {
        if (s.isEmpty()) {
            return false;
        }
        for (int i = 0; i < s.length(); i++) {
            if (!p.test(s.charAt(i))) {
                return false;
            }
        }
        return true;
    }

    // ---- helper types for reflection / builder / enum sections ----
    enum Color { RED, GREEN, BLUE }

    static final class Bean {
        private String name;
        private int age;

        Bean(String name, int age) {
            this.name = name;
            this.age = age;
        }

        public String greet(String who) {
            return name + ":" + who;
        }
    }

    // =========================================================================
    public static void main(String[] args) {
        // Make every locale/timezone-sensitive API deterministic.
        TimeZone.setDefault(TimeZone.getTimeZone("UTC"));
        Locale.setDefault(Locale.US);

        stringUtilsSection();
        randomSection();
        arraySection();
        numberSection();
        objectSection();
        builderSection();
        tupleSection();
        rangeSection();
        validateSection();
        classSection();
        systemSection();
        wordSection();
        charSection();
        booleanSection();
        reflectSection();
        textSection();
        timeSection();
        exceptionSection();
        mutableSection();
        fractionSection();
        enumSection();
        comparableSection();
        charSetSection();
        escapeSection();
        regexSection();

        System.out.println("LANG3_RESULT ok=" + ok + " fail=" + fail);
        if (fail == 0) {
            System.out.println("LANG3_DONE");
        }
    }

    // =========================================================================
    private static void stringUtilsSection() {
        // empty / blank predicates
        tru("isBlank.spaces", StringUtils.isBlank("   "));
        tru("isBlank.null", StringUtils.isBlank(null));
        tru("isBlank.empty", StringUtils.isBlank(""));
        fls("isBlank.text", StringUtils.isBlank(" a "));
        tru("isEmpty.empty", StringUtils.isEmpty(""));
        tru("isEmpty.null", StringUtils.isEmpty(null));
        fls("isEmpty.space", StringUtils.isEmpty(" "));
        tru("isNotBlank", StringUtils.isNotBlank("x"));
        tru("isNotEmpty", StringUtils.isNotEmpty("x"));

        // classification
        tru("isNumeric.digits", StringUtils.isNumeric("123"));
        fls("isNumeric.dot", StringUtils.isNumeric("12.3"));
        fls("isNumeric.empty", StringUtils.isNumeric(""));
        tru("isAlpha", StringUtils.isAlpha("abcXYZ"));
        fls("isAlpha.mixed", StringUtils.isAlpha("ab1"));
        tru("isAlphanumeric", StringUtils.isAlphanumeric("ab12"));
        fls("isAlphanumeric.space", StringUtils.isAlphanumeric("ab 12"));

        // trim / strip
        eq("trim", "abc", StringUtils.trim("  abc  "));
        eq("trim.null", null, StringUtils.trim(null));
        eq("strip", "abc", StringUtils.strip("  abc  "));
        eq("strip.chars", "abc", StringUtils.strip("xxabcxx", "x"));
        eq("stripStart", "abc  ", StringUtils.stripStart("  abc  ", null));
        eq("stripEnd", "  abc", StringUtils.stripEnd("  abc  ", null));

        // case
        eq("capitalize", "Cat", StringUtils.capitalize("cat"));
        eq("capitalize.empty", "", StringUtils.capitalize(""));
        eq("uncapitalize", "cat", StringUtils.uncapitalize("Cat"));
        eq("swapCase", "hELLO wORLD", StringUtils.swapCase("Hello World"));
        eq("lowerCase", "abc", StringUtils.lowerCase("ABC"));
        eq("upperCase", "ABC", StringUtils.upperCase("abc"));

        // padding / abbreviate / center
        eq("leftPad", "005", StringUtils.leftPad("5", 3, '0'));
        eq("leftPad.short", "abc", StringUtils.leftPad("abc", 2, '0'));
        eq("rightPad", "500", StringUtils.rightPad("5", 3, '0'));
        eq("abbreviate4", "a...", StringUtils.abbreviate("abcdefg", 4));
        eq("abbreviate6", "abc...", StringUtils.abbreviate("abcdefg", 6));
        eq("center", "  abc  ", StringUtils.center("abc", 7));
        eq("center.char", "**abc**", StringUtils.center("abc", 7, '*'));
        eq("center.short", "abc", StringUtils.center("abc", 2));

        // repeat / reverse
        eq("repeat", "ababab", StringUtils.repeat("ab", 3));
        eq("repeat.zero", "", StringUtils.repeat("ab", 0));
        eq("reverse", "tab", StringUtils.reverse("bat"));
        eq("reverse.empty", "", StringUtils.reverse(""));

        // split / join
        eqArr("split.colon", new String[] {"a", "b", "c"}, StringUtils.split("a:b:c", ':'));
        eqArr("split.collapse", new String[] {"a", "b"}, StringUtils.split("a..b", '.'));
        eqArr("splitByWholeSeparator", new String[] {"a", "b", "c"},
                StringUtils.splitByWholeSeparator("a..b..c", ".."));
        eq("join.list", "a,b,c", StringUtils.join(Arrays.asList("a", "b", "c"), ","));
        eq("join.intArr", "1-2-3", StringUtils.join(new int[] {1, 2, 3}, '-'));

        // substring family
        eq("substringBetween", "abc", StringUtils.substringBetween("[abc]", "[", "]"));
        eq("substringBetween.tag", "abc", StringUtils.substringBetween("YabcY", "Y"));
        eq("substringBefore", "a", StringUtils.substringBefore("a.b.c", "."));
        eq("substringAfter", "b.c", StringUtils.substringAfter("a.b.c", "."));
        eq("substring.range", "cd", StringUtils.substring("abcdef", 2, 4));
        eq("left", "abc", StringUtils.left("abcdef", 3));
        eq("right", "def", StringUtils.right("abcdef", 3));
        eq("mid", "cd", StringUtils.mid("abcdef", 2, 2));

        // count / contains / startsWith / endsWith
        eqi("countMatches.cs", 3, StringUtils.countMatches("ababab", "ab"));
        eqi("countMatches.char", 3, StringUtils.countMatches("aaa", 'a'));
        tru("contains", StringUtils.contains("abc", "b"));
        fls("contains.no", StringUtils.contains("abc", "z"));
        tru("containsIgnoreCase", StringUtils.containsIgnoreCase("ABC", "b"));
        tru("startsWith", StringUtils.startsWith("abcdef", "abc"));
        fls("startsWith.no", StringUtils.startsWith("abcdef", "xyz"));
        tru("endsWith", StringUtils.endsWith("abcdef", "def"));
        tru("equalsIgnoreCase", StringUtils.equalsIgnoreCase("abc", "ABC"));
        eqi("indexOf", 2, StringUtils.indexOf("aabaa", 'b'));

        // difference / levenshtein
        eq("difference", "var", StringUtils.difference("foobar", "foovar"));
        eq("difference.empty", "abc", StringUtils.difference("", "abc"));
        eq("difference.same", "", StringUtils.difference("abc", "abc"));
        eqi("levenshtein", 3, StringUtils.getLevenshteinDistance("kitten", "sitting"));
        eqi("levenshtein.empty", 3, StringUtils.getLevenshteinDistance("", "abc"));

        // wrap / remove / replace / replaceEach
        eq("wrap.char", "xabx", StringUtils.wrap("ab", 'x'));
        eq("wrap.str", "'ab'", StringUtils.wrap("ab", "'"));
        eq("remove.cs", "qd", StringUtils.remove("queued", "ue"));
        eq("remove.char", "heo", StringUtils.remove("hello", 'l'));
        eq("removeStart", "domain.com", StringUtils.removeStart("www.domain.com", "www."));
        eq("removeEnd", "a", StringUtils.removeEnd("a.txt", ".txt"));
        eq("replace", "zbz", StringUtils.replace("aba", "a", "z"));
        eq("replace.none", "abc", StringUtils.replace("abc", "x", "y"));
        eq("replaceEach", "hi earth",
                StringUtils.replaceEach("hello world",
                        new String[] {"hello", "world"}, new String[] {"hi", "earth"}));

        // defaults / misc
        eq("defaultIfBlank.empty", "def", StringUtils.defaultIfBlank("", "def"));
        eq("defaultIfBlank.blank", "def", StringUtils.defaultIfBlank("   ", "def"));
        eq("defaultIfBlank.value", "x", StringUtils.defaultIfBlank("x", "def"));
        eq("defaultString.null", "", StringUtils.defaultString(null));
        eq("defaultString.value", "a", StringUtils.defaultString("a"));
        eq("chomp", "abc", StringUtils.chomp("abc\r\n"));
        eq("chop", "ab", StringUtils.chop("abc"));
        eq("deleteWhitespace", "abc", StringUtils.deleteWhitespace("a b\tc"));
        eq("normalizeSpace", "a b", StringUtils.normalizeSpace("  a   b  "));
        eq("appendIfMissing", "a.txt", StringUtils.appendIfMissing("a", ".txt"));
        eq("appendIfMissing.present", "a.txt", StringUtils.appendIfMissing("a.txt", ".txt"));
        eq("prependIfMissing", "/dir", StringUtils.prependIfMissing("dir", "/"));
    }

    // =========================================================================
    private static void randomSection() {
        String num = RandomStringUtils.randomNumeric(10);
        eqi("randomNumeric.len", 10, num.length());
        tru("randomNumeric.digits", allMatch(num, Character::isDigit));

        String alpha = RandomStringUtils.randomAlphabetic(8);
        eqi("randomAlphabetic.len", 8, alpha.length());
        tru("randomAlphabetic.letters", allMatch(alpha, Character::isLetter));

        String alnum = RandomStringUtils.randomAlphanumeric(12);
        eqi("randomAlphanumeric.len", 12, alnum.length());
        tru("randomAlphanumeric.alnum", allMatch(alnum, Character::isLetterOrDigit));

        String fromSet = RandomStringUtils.random(20, "abc");
        eqi("random.set.len", 20, fromSet.length());
        tru("random.set.members", allMatch(fromSet, c -> c == 'a' || c == 'b' || c == 'c'));

        eqi("random.zero", 0, RandomStringUtils.random(0).length());

        String ascii = RandomStringUtils.randomAscii(16);
        eqi("randomAscii.len", 16, ascii.length());
        tru("randomAscii.printable", allMatch(ascii, c -> c >= 32 && c <= 126));

        // RandomUtils deterministic edges (start == end returns start)
        eqi("RandomUtils.nextInt.eq", 5, RandomUtils.nextInt(5, 5));
        eql("RandomUtils.nextLong.eq", 10L, RandomUtils.nextLong(10L, 10L));
        eqd("RandomUtils.nextDouble.eq", 1.5, RandomUtils.nextDouble(1.5, 1.5), 0.0);
        tru("RandomUtils.nextInt.nonneg", RandomUtils.nextInt() >= 0);
        int rr = RandomUtils.nextInt(3, 9);
        tru("RandomUtils.nextInt.range", rr >= 3 && rr < 9);
        eqi("RandomUtils.nextBytes.len", 4, RandomUtils.nextBytes(4).length);
    }

    // =========================================================================
    private static void arraySection() {
        eqArr("ArrayUtils.add", new int[] {1, 2, 3}, ArrayUtils.add(new int[] {1, 2}, 3));
        eqArr("ArrayUtils.addAll", new int[] {1, 2, 3},
                ArrayUtils.addAll(new int[] {1}, 2, 3));
        eqArr("ArrayUtils.remove", new int[] {1, 3}, ArrayUtils.remove(new int[] {1, 2, 3}, 1));
        eqArr("ArrayUtils.removeElement", new int[] {1, 3},
                ArrayUtils.removeElement(new int[] {1, 2, 3}, 2));
        tru("ArrayUtils.contains", ArrayUtils.contains(new int[] {1, 2, 3}, 2));
        fls("ArrayUtils.contains.no", ArrayUtils.contains(new int[] {1, 2, 3}, 9));
        eqi("ArrayUtils.indexOf", 1, ArrayUtils.indexOf(new int[] {1, 2, 3}, 2));
        eqi("ArrayUtils.indexOf.miss", ArrayUtils.INDEX_NOT_FOUND,
                ArrayUtils.indexOf(new int[] {1, 2, 3}, 9));
        eqArr("ArrayUtils.subarray", new int[] {2, 3},
                ArrayUtils.subarray(new int[] {1, 2, 3, 4, 5}, 1, 3));

        int[] toRev = {1, 2, 3};
        ArrayUtils.reverse(toRev);
        eqArr("ArrayUtils.reverse", new int[] {3, 2, 1}, toRev);

        eqArr("ArrayUtils.toObject", new Integer[] {1, 2, 3},
                ArrayUtils.toObject(new int[] {1, 2, 3}));
        eqArr("ArrayUtils.toPrimitive", new int[] {4, 5},
                ArrayUtils.toPrimitive(new Integer[] {4, 5}));

        tru("ArrayUtils.isEmpty", ArrayUtils.isEmpty(new int[] {}));
        fls("ArrayUtils.isEmpty.no", ArrayUtils.isEmpty(new int[] {1}));
        eqi("ArrayUtils.nullToEmpty", 0, ArrayUtils.nullToEmpty((int[]) null).length);
        eqi("ArrayUtils.getLength", 3, ArrayUtils.getLength(new int[] {1, 2, 3}));

        int[] src = {7, 8, 9};
        int[] cl = ArrayUtils.clone(src);
        eqArr("ArrayUtils.clone.equal", src, cl);
        fls("ArrayUtils.clone.distinct", src == cl);
    }

    // =========================================================================
    private static void numberSection() {
        eqi("NumberUtils.toInt", 123, NumberUtils.toInt("123"));
        eqi("NumberUtils.toInt.bad", 0, NumberUtils.toInt("abc"));
        eqi("NumberUtils.toInt.default", 7, NumberUtils.toInt("abc", 7));
        eql("NumberUtils.toLong", 123L, NumberUtils.toLong("123"));
        eql("NumberUtils.toLong.default", 5L, NumberUtils.toLong("x", 5L));
        eqd("NumberUtils.toDouble", 1.5, NumberUtils.toDouble("1.5"), 0.0);
        eqd("NumberUtils.toDouble.default", 2.0, NumberUtils.toDouble("x", 2.0), 0.0);

        eqi("NumberUtils.createInteger.hex", 31, NumberUtils.createInteger("0x1F").intValue());
        eqi("NumberUtils.createInteger.dec", 123, NumberUtils.createInteger("123").intValue());
        eqd("NumberUtils.createNumber.dec", 1.5, NumberUtils.createNumber("1.5").doubleValue(), 0.0);
        eqi("NumberUtils.createNumber.hex", 255, NumberUtils.createNumber("0xFF").intValue());
        tru("NumberUtils.createNumber.intType", NumberUtils.createNumber("123") instanceof Integer);

        tru("NumberUtils.isCreatable.int", NumberUtils.isCreatable("123"));
        tru("NumberUtils.isCreatable.sci", NumberUtils.isCreatable("1.5e3"));
        fls("NumberUtils.isCreatable.text", NumberUtils.isCreatable("abc"));
        fls("NumberUtils.isCreatable.empty", NumberUtils.isCreatable(""));
        tru("NumberUtils.isDigits", NumberUtils.isDigits("123"));
        fls("NumberUtils.isDigits.dot", NumberUtils.isDigits("12.3"));
        fls("NumberUtils.isDigits.empty", NumberUtils.isDigits(""));

        eqi("NumberUtils.max.triple", 3, NumberUtils.max(1, 2, 3));
        eqi("NumberUtils.min.triple", 1, NumberUtils.min(1, 2, 3));
        eqi("NumberUtils.max.arr", 3, NumberUtils.max(new int[] {3, 1, 2}));
        eqi("NumberUtils.min.arr", 1, NumberUtils.min(new int[] {3, 1, 2}));
        eqd("NumberUtils.max.double", 2.5, NumberUtils.max(1.5, 2.5, 0.5), 0.0);

        eqi("NumberUtils.compare.lt", -1, NumberUtils.compare(1, 2));
        eqi("NumberUtils.compare.eq", 0, NumberUtils.compare(2, 2));
        eqi("NumberUtils.compare.gt", 1, NumberUtils.compare(3, 2));
    }

    // =========================================================================
    private static void objectSection() {
        eq("ObjectUtils.defaultIfNull.null", "d", ObjectUtils.defaultIfNull(null, "d"));
        eq("ObjectUtils.defaultIfNull.value", "x", ObjectUtils.defaultIfNull("x", "d"));
        eq("ObjectUtils.firstNonNull", "a", ObjectUtils.firstNonNull(null, null, "a", "b"));
        eq("ObjectUtils.firstNonNull.allNull", null, ObjectUtils.firstNonNull((Object) null, null));
        tru("ObjectUtils.allNotNull.true", ObjectUtils.allNotNull("a", "b"));
        fls("ObjectUtils.allNotNull.false", ObjectUtils.allNotNull("a", null));
        tru("ObjectUtils.anyNotNull.true", ObjectUtils.anyNotNull(null, "a"));
        fls("ObjectUtils.anyNotNull.false", ObjectUtils.anyNotNull(null, null));
        tru("ObjectUtils.identityToString.prefix",
                ObjectUtils.identityToString(new Object()).startsWith("java.lang.Object@"));
        tru("ObjectUtils.isEmpty.emptyStr", ObjectUtils.isEmpty(""));
        fls("ObjectUtils.isEmpty.value", ObjectUtils.isEmpty("a"));
        tru("ObjectUtils.isNotEmpty", ObjectUtils.isNotEmpty("a"));
        eq("ObjectUtils.max", Integer.valueOf(9),
                ObjectUtils.max(Integer.valueOf(3), Integer.valueOf(9), Integer.valueOf(5)));
        eq("ObjectUtils.min", Integer.valueOf(3),
                ObjectUtils.min(Integer.valueOf(3), Integer.valueOf(9), Integer.valueOf(5)));
    }

    // =========================================================================
    private static void builderSection() {
        Bean b = new Bean("Ann", 30);
        String ts = new ToStringBuilder(b, ToStringStyle.NO_FIELD_NAMES_STYLE)
                .append(b.name).append(b.age).toString();
        tru("ToStringBuilder.value", ts.contains("Ann") && ts.contains("30"));

        String refl = ReflectionToStringBuilder.toString(b, ToStringStyle.SHORT_PREFIX_STYLE);
        tru("ReflectionToStringBuilder.name", refl.contains("name=Ann"));
        tru("ReflectionToStringBuilder.age", refl.contains("age=30"));

        Bean b2 = new Bean("Ann", 30);
        Bean b3 = new Bean("Bob", 40);
        tru("EqualsBuilder.reflectionEquals.same", EqualsBuilder.reflectionEquals(b, b2));
        fls("EqualsBuilder.reflectionEquals.diff", EqualsBuilder.reflectionEquals(b, b3));
        tru("EqualsBuilder.append.true",
                new EqualsBuilder().append(1, 1).append("a", "a").isEquals());
        fls("EqualsBuilder.append.false",
                new EqualsBuilder().append(1, 1).append("a", "b").isEquals());

        eqi("HashCodeBuilder.reflectionEqual",
                HashCodeBuilder.reflectionHashCode(b), HashCodeBuilder.reflectionHashCode(b2));
        eqi("HashCodeBuilder.append.equal",
                new HashCodeBuilder(17, 37).append("a").append(1).toHashCode(),
                new HashCodeBuilder(17, 37).append("a").append(1).toHashCode());

        tru("CompareToBuilder.lt", new CompareToBuilder().append(1, 2).toComparison() < 0);
        eqi("CompareToBuilder.eq", 0, new CompareToBuilder().append(5, 5).toComparison());
        tru("CompareToBuilder.gt", new CompareToBuilder().append(9, 2).toComparison() > 0);
    }

    // =========================================================================
    private static void tupleSection() {
        Pair<String, Integer> p = Pair.of("k", 7);
        eq("Pair.getLeft", "k", p.getLeft());
        eq("Pair.getRight", Integer.valueOf(7), p.getRight());
        eq("Pair.getKey", "k", p.getKey());
        eq("Pair.getValue", Integer.valueOf(7), p.getValue());
        eq("Pair.toString", "(a,b)", Pair.of("a", "b").toString());
        tru("Pair.equals", Pair.of(1, 2).equals(Pair.of(1, 2)));
        fls("Pair.notEquals", Pair.of(1, 2).equals(Pair.of(1, 3)));

        Triple<Integer, Integer, Integer> t = Triple.of(1, 2, 3);
        eq("Triple.left", Integer.valueOf(1), t.getLeft());
        eq("Triple.middle", Integer.valueOf(2), t.getMiddle());
        eq("Triple.right", Integer.valueOf(3), t.getRight());
        eq("Triple.toString", "(1,2,3)", t.toString());

        MutablePair<String, String> mp = MutablePair.of("a", "b");
        mp.setLeft("z");
        mp.setRight("y");
        eq("MutablePair.setLeft", "z", mp.getLeft());
        eq("MutablePair.setRight", "y", mp.getRight());

        ImmutablePair<Integer, Integer> ip = ImmutablePair.of(4, 5);
        eq("ImmutablePair.left", Integer.valueOf(4), ip.getLeft());
        eq("ImmutablePair.right", Integer.valueOf(5), ip.getRight());
        eq("ImmutablePair.toString", "(1,2)", ImmutablePair.of(1, 2).toString());
    }

    // =========================================================================
    private static void rangeSection() {
        Range<Integer> r = Range.between(1, 10);
        eq("Range.min", Integer.valueOf(1), r.getMinimum());
        eq("Range.max", Integer.valueOf(10), r.getMaximum());
        tru("Range.contains.in", r.contains(5));
        fls("Range.contains.out", r.contains(11));
        tru("Range.contains.lo", r.contains(1));
        tru("Range.contains.hi", r.contains(10));

        tru("Range.overlap.yes", r.isOverlappedBy(Range.between(5, 15)));
        fls("Range.overlap.no", r.isOverlappedBy(Range.between(11, 20)));

        Range<Integer> inter = r.intersectionWith(Range.between(5, 15));
        eq("Range.intersection.min", Integer.valueOf(5), inter.getMinimum());
        eq("Range.intersection.max", Integer.valueOf(10), inter.getMaximum());

        eq("Range.fit.high", Integer.valueOf(10), r.fit(15));
        eq("Range.fit.low", Integer.valueOf(1), r.fit(-3));
        eq("Range.fit.in", Integer.valueOf(5), r.fit(5));

        tru("Range.isBefore", r.isBefore(20));
        tru("Range.isAfter", r.isAfter(-5));
        eqi("Range.elementCompareTo.below", -1, r.elementCompareTo(0));
        eqi("Range.elementCompareTo.in", 0, r.elementCompareTo(5));
        eqi("Range.elementCompareTo.above", 1, r.elementCompareTo(20));

        // arguments are auto-ordered
        eq("Range.reversed.min", Integer.valueOf(1), Range.between(10, 1).getMinimum());
    }

    // =========================================================================
    private static void validateSection() {
        // success paths return the value / do not throw
        Validate.isTrue(true);
        ok++;
        eq("Validate.notNull.returns", "x", Validate.notNull("x"));
        eq("Validate.notEmpty.returns", "x", Validate.notEmpty("x"));
        eq("Validate.notBlank.returns", "x", Validate.notBlank("x"));
        Validate.inclusiveBetween(1L, 10L, 5L);
        ok++;
        Validate.notEmpty(Collections.singletonList(1));
        ok++;

        // failure paths
        throwsType("Validate.isTrue.throws", IllegalArgumentException.class,
                () -> Validate.isTrue(false, "must be true"));
        throwsType("Validate.notNull.throws", NullPointerException.class,
                () -> Validate.notNull(null, "null not allowed"));
        throwsType("Validate.notEmpty.str.throws", IllegalArgumentException.class,
                () -> Validate.notEmpty(""));
        throwsType("Validate.notBlank.throws", IllegalArgumentException.class,
                () -> Validate.notBlank("   "));
        throwsType("Validate.notEmpty.coll.throws", IllegalArgumentException.class,
                () -> Validate.notEmpty(Collections.emptyList()));
        throwsType("Validate.inclusiveBetween.throws", IllegalArgumentException.class,
                () -> Validate.inclusiveBetween(1L, 10L, 20L));
    }

    // =========================================================================
    private static void classSection() {
        eq("ClassUtils.getShortClassName.class", "ArrayList",
                ClassUtils.getShortClassName(ArrayList.class));
        eq("ClassUtils.getShortClassName.str", "ArrayList",
                ClassUtils.getShortClassName("java.util.ArrayList"));
        eq("ClassUtils.getPackageName.class", "java.util",
                ClassUtils.getPackageName(ArrayList.class));
        eq("ClassUtils.getPackageName.str", "java.util",
                ClassUtils.getPackageName("java.util.ArrayList"));
        List<Class<?>> ifaces = ClassUtils.getAllInterfaces(ArrayList.class);
        tru("ClassUtils.getAllInterfaces.List", ifaces.contains(List.class));
        tru("ClassUtils.isAssignable.up", ClassUtils.isAssignable(Integer.class, Number.class));
        fls("ClassUtils.isAssignable.down", ClassUtils.isAssignable(Number.class, Integer.class));
    }

    // =========================================================================
    private static void systemSection() {
        tru("SystemUtils.JAVA_VERSION", StringUtils.isNotBlank(SystemUtils.JAVA_VERSION));
        tru("SystemUtils.JAVA_HOME", SystemUtils.getJavaHome() != null);
        tru("SystemUtils.JAVA_SPEC", StringUtils.isNotBlank(SystemUtils.JAVA_SPECIFICATION_VERSION));
        tru("SystemUtils.atLeast.8", SystemUtils.isJavaVersionAtLeast(JavaVersion.JAVA_1_8));
        tru("SystemUtils.atLeast.17", SystemUtils.isJavaVersionAtLeast(JavaVersion.JAVA_17));
        tru("SystemUtils.userDir", SystemUtils.getUserDir() != null);
        tru("SystemUtils.fileSeparator", StringUtils.isNotEmpty(SystemUtils.FILE_SEPARATOR));
    }

    // =========================================================================
    private static void wordSection() {
        eq("WordUtils.capitalize", "Hello World", WordUtils.capitalize("hello world"));
        eq("WordUtils.capitalizeFully", "Hello World", WordUtils.capitalizeFully("hELLO wORLD"));
        eq("WordUtils.uncapitalize", "hello world", WordUtils.uncapitalize("Hello World"));
        eq("WordUtils.initials", "HW", WordUtils.initials("Hello World"));
        eq("WordUtils.swapCase", "hELLO", WordUtils.swapCase("Hello"));
        String wrapped = WordUtils.wrap("aaa bbb ccc ddd", 7);
        eq("WordUtils.wrap.exact", "aaa bbb\nccc ddd", wrapped);
        eqi("WordUtils.wrap.lines", 2, StringUtils.countMatches(wrapped, "\n") + 1);
    }

    // =========================================================================
    private static void charSection() {
        tru("CharUtils.isAscii.a", CharUtils.isAscii('a'));
        fls("CharUtils.isAscii.hi", CharUtils.isAscii((char) 200));
        tru("CharUtils.isAsciiAlpha", CharUtils.isAsciiAlpha('a'));
        fls("CharUtils.isAsciiAlpha.digit", CharUtils.isAsciiAlpha('1'));
        tru("CharUtils.isAsciiNumeric", CharUtils.isAsciiNumeric('5'));
        fls("CharUtils.isAsciiNumeric.alpha", CharUtils.isAsciiNumeric('a'));
        tru("CharUtils.isAsciiAlphanumeric", CharUtils.isAsciiAlphanumeric('z'));
        eqi("CharUtils.toChar", 'a', CharUtils.toChar("a"));
        eqi("CharUtils.toChar.Character", 'b', CharUtils.toChar(Character.valueOf('b')));
        eqi("CharUtils.toIntValue", 7, CharUtils.toIntValue('7'));
        eq("CharUtils.toString", "a", CharUtils.toString('a'));
        eq("CharUtils.unicodeEscaped", "\\u0041", CharUtils.unicodeEscaped('A'));
    }

    // =========================================================================
    private static void booleanSection() {
        tru("BooleanUtils.toBoolean.true", BooleanUtils.toBoolean("true"));
        tru("BooleanUtils.toBoolean.yes", BooleanUtils.toBoolean("yes"));
        tru("BooleanUtils.toBoolean.on", BooleanUtils.toBoolean("on"));
        fls("BooleanUtils.toBoolean.false", BooleanUtils.toBoolean("false"));
        fls("BooleanUtils.toBoolean.junk", BooleanUtils.toBoolean("blah"));
        tru("BooleanUtils.toBoolean.int1", BooleanUtils.toBoolean(1));
        fls("BooleanUtils.toBoolean.int0", BooleanUtils.toBoolean(0));
        eqi("BooleanUtils.toInteger.true", 1, BooleanUtils.toInteger(true));
        eqi("BooleanUtils.toInteger.false", 0, BooleanUtils.toInteger(false));
        eq("BooleanUtils.toStringYesNo", "yes", BooleanUtils.toStringYesNo(true));
        eq("BooleanUtils.toStringTrueFalse", "false", BooleanUtils.toStringTrueFalse(false));
        eq("BooleanUtils.toStringOnOff", "on", BooleanUtils.toStringOnOff(true));
        eq("BooleanUtils.negate.true", Boolean.FALSE, BooleanUtils.negate(Boolean.TRUE));
        eq("BooleanUtils.negate.null", null, BooleanUtils.negate(null));
        tru("BooleanUtils.and.allTrue", BooleanUtils.and(new boolean[] {true, true, true}));
        fls("BooleanUtils.and.oneFalse", BooleanUtils.and(new boolean[] {true, false, true}));
        tru("BooleanUtils.or.oneTrue", BooleanUtils.or(new boolean[] {false, false, true}));
        fls("BooleanUtils.or.allFalse", BooleanUtils.or(new boolean[] {false, false}));
        tru("BooleanUtils.xor.diff", BooleanUtils.xor(new boolean[] {true, false}));
        fls("BooleanUtils.xor.same", BooleanUtils.xor(new boolean[] {true, true}));
        tru("BooleanUtils.isTrue", BooleanUtils.isTrue(Boolean.TRUE));
        fls("BooleanUtils.isTrue.null", BooleanUtils.isTrue(null));
        tru("BooleanUtils.isFalse", BooleanUtils.isFalse(Boolean.FALSE));
        eq("BooleanUtils.toBooleanObject", Boolean.TRUE, BooleanUtils.toBooleanObject("true"));
        eq("BooleanUtils.toBooleanObject.null", null, BooleanUtils.toBooleanObject("xyz"));
    }

    // =========================================================================
    private static void reflectSection() {
        Bean b = new Bean("Eve", 22);
        try {
            eq("FieldUtils.readField", "Eve", FieldUtils.readField(b, "name", true));
            eq("FieldUtils.readField.int", Integer.valueOf(22), FieldUtils.readField(b, "age", true));
            FieldUtils.writeField(b, "age", 99, true);
            eq("FieldUtils.writeField", Integer.valueOf(99), FieldUtils.readField(b, "age", true));
        } catch (IllegalAccessException e) {
            fail++;
            System.out.println("FAIL FieldUtils.access " + e);
        }

        try {
            eq("MethodUtils.invokeMethod.toUpper", "HELLO",
                    MethodUtils.invokeMethod("hello", "toUpperCase"));
            eq("MethodUtils.invokeMethod.concat", "hello!",
                    MethodUtils.invokeMethod("hello", "concat", "!"));
            eq("MethodUtils.invokeMethod.bean", "Eve:bob",
                    MethodUtils.invokeMethod(b, "greet", "bob"));
        } catch (ReflectiveOperationException e) {
            fail++;
            System.out.println("FAIL MethodUtils.invoke " + e);
        }
    }

    // =========================================================================
    private static void textSection() {
        Map<String, String> m = new HashMap<>();
        m.put("name", "World");
        eq("StrSubstitutor.instance", "Hello World!",
                new StrSubstitutor(m).replace("Hello ${name}!"));
        eq("StrSubstitutor.static", "a=1",
                StrSubstitutor.replace("a=${v}", Collections.singletonMap("v", "1")));

        StrBuilder sb = new StrBuilder();
        sb.append("a").append("b").append("c");
        eq("StrBuilder.toString", "abc", sb.toString());
        eqi("StrBuilder.size", 3, sb.size());
        eq("StrBuilder.reverse", "cba", sb.reverse().toString());

        StrTokenizer tok = new StrTokenizer("a b c");
        eqArr("StrTokenizer.tokens", new String[] {"a", "b", "c"}, tok.getTokenArray());
        StrTokenizer csv = new StrTokenizer("x,y,z", ',');
        eqi("StrTokenizer.csv.size", 3, csv.size());
    }

    // =========================================================================
    private static void timeSection() {
        StopWatch sw = StopWatch.create();
        fls("StopWatch.notStarted", sw.isStarted());
        sw.start();
        tru("StopWatch.started", sw.isStarted());
        sw.split();
        tru("StopWatch.splitNonNeg", sw.getSplitTime() >= 0);
        sw.stop();
        tru("StopWatch.stopped", sw.isStopped());
        tru("StopWatch.timeNonNeg", sw.getTime() >= 0);

        eq("DurationFormatUtils.HMS.zero", "00:00:00.000", DurationFormatUtils.formatDurationHMS(0));
        eq("DurationFormatUtils.ss", "01", DurationFormatUtils.formatDuration(1000L, "ss"));
        eq("DurationFormatUtils.mss", "1:05", DurationFormatUtils.formatDuration(65000L, "m:ss"));
        eq("DurationFormatUtils.Hmmss", "1:01:01",
                DurationFormatUtils.formatDuration(3661000L, "H:mm:ss"));

        // UTC fixed: epoch arithmetic is timezone-independent here
        eql("DateUtils.addDays", 86400000L, DateUtils.addDays(new Date(0), 1).getTime());
        eql("DateUtils.addHours", 7200000L, DateUtils.addHours(new Date(0), 2).getTime());
        tru("DateUtils.isSameDay.true", DateUtils.isSameDay(new Date(0), new Date(3600000L)));
        fls("DateUtils.isSameDay.false", DateUtils.isSameDay(new Date(0), new Date(90000000L)));
        eql("DateUtils.truncate.hour", 3600000L,
                DateUtils.truncate(new Date(3661000L), Calendar.HOUR_OF_DAY).getTime());
        eq("DateFormatUtils.format", "1970-01-01", DateFormatUtils.format(new Date(0), "yyyy-MM-dd"));
    }

    // =========================================================================
    private static void exceptionSection() {
        Throwable inner = new IllegalStateException("inner boom");
        Throwable outer = new RuntimeException("outer wrap", inner);

        eq("ExceptionUtils.getRootCause", inner, ExceptionUtils.getRootCause(outer));
        tru("ExceptionUtils.getRootCauseMessage",
                ExceptionUtils.getRootCauseMessage(outer).contains("inner boom"));
        eqi("ExceptionUtils.getThrowableCount", 2, ExceptionUtils.getThrowableCount(outer));
        eqi("ExceptionUtils.indexOfThrowable", 1,
                ExceptionUtils.indexOfThrowable(outer, IllegalStateException.class));
        eqi("ExceptionUtils.getThrowableList", 2, ExceptionUtils.getThrowableList(outer).size());
        tru("ExceptionUtils.getStackTrace",
                ExceptionUtils.getStackTrace(inner).contains("IllegalStateException"));
        eq("ExceptionUtils.getMessage", "IllegalStateException: inner boom",
                ExceptionUtils.getMessage(inner));
    }

    // =========================================================================
    private static void mutableSection() {
        MutableInt mi = new MutableInt(5);
        mi.increment();
        eqi("MutableInt.increment", 6, mi.intValue());
        mi.add(4);
        eqi("MutableInt.add", 10, mi.intValue());
        eq("MutableInt.getValue", Integer.valueOf(10), mi.getValue());
        eqi("MutableInt.incrementAndGet", 11, mi.incrementAndGet());
        eqi("MutableInt.getAndAdd", 11, mi.getAndAdd(9));
        eqi("MutableInt.afterGetAndAdd", 20, mi.intValue());

        MutableObject<String> mo = new MutableObject<>("a");
        eq("MutableObject.initial", "a", mo.getValue());
        mo.setValue("b");
        eq("MutableObject.set", "b", mo.getValue());
    }

    // =========================================================================
    private static void fractionSection() {
        Fraction sum = Fraction.getFraction(1, 2).add(Fraction.getFraction(1, 3));
        eqi("Fraction.add.num", 5, sum.getNumerator());
        eqi("Fraction.add.den", 6, sum.getDenominator());
        Fraction red = Fraction.getFraction(2, 4).reduce();
        eqi("Fraction.reduce.num", 1, red.getNumerator());
        eqi("Fraction.reduce.den", 2, red.getDenominator());
        eqd("Fraction.doubleValue", 0.5, Fraction.getFraction(1, 2).doubleValue(), 0.0);
        eqi("Fraction.negate.num", -3, Fraction.getFraction(3, 4).negate().getNumerator());
    }

    // =========================================================================
    private static void enumSection() {
        eq("EnumUtils.getEnum", Color.RED, EnumUtils.getEnum(Color.class, "RED"));
        eq("EnumUtils.getEnum.missing", null, EnumUtils.getEnum(Color.class, "PURPLE"));
        tru("EnumUtils.isValidEnum", EnumUtils.isValidEnum(Color.class, "GREEN"));
        fls("EnumUtils.isValidEnum.no", EnumUtils.isValidEnum(Color.class, "PURPLE"));
        eqi("EnumUtils.getEnumList.size", 3, EnumUtils.getEnumList(Color.class).size());
        eq("EnumUtils.getEnumIgnoreCase", Color.BLUE,
                EnumUtils.getEnumIgnoreCase(Color.class, "blue"));
        eqi("EnumUtils.getEnumMap.size", 3, EnumUtils.getEnumMap(Color.class).size());
    }

    // =========================================================================
    private static void comparableSection() {
        tru("ComparableUtils.is.between.in", ComparableUtils.is(5).between(1, 10));
        fls("ComparableUtils.is.between.out", ComparableUtils.is(15).between(1, 10));
        eq("ComparableUtils.max", Integer.valueOf(7), ComparableUtils.max(3, 7));
        eq("ComparableUtils.min", Integer.valueOf(3), ComparableUtils.min(3, 7));
        Predicate<Integer> between = ComparableUtils.between(1, 10);
        tru("ComparableUtils.between.predicate.in", between.test(5));
        fls("ComparableUtils.between.predicate.out", between.test(20));
    }

    // =========================================================================
    private static void charSetSection() {
        eqi("CharSetUtils.count", 2, CharSetUtils.count("hello", "l"));
        eq("CharSetUtils.delete", "heo", CharSetUtils.delete("hello", "l"));
        eq("CharSetUtils.keep", "ll", CharSetUtils.keep("hello", "l"));
        eq("CharSetUtils.squeeze", "helo", CharSetUtils.squeeze("hello", "l"));
    }

    // =========================================================================
    private static void escapeSection() {
        eq("StringEscapeUtils.escapeJava.tab", "a\\tb", StringEscapeUtils.escapeJava("a\tb"));
        eq("StringEscapeUtils.escapeJava.quote", "a\\\"b", StringEscapeUtils.escapeJava("a\"b"));
        eq("StringEscapeUtils.unescapeJava.tab", "a\tb", StringEscapeUtils.unescapeJava("a\\tb"));
        eq("StringEscapeUtils.escapeHtml4", "&lt;a&gt;&amp;", StringEscapeUtils.escapeHtml4("<a>&"));
        eq("StringEscapeUtils.unescapeHtml4", "<", StringEscapeUtils.unescapeHtml4("&lt;"));
        eq("StringEscapeUtils.escapeCsv", "\"a,b\"", StringEscapeUtils.escapeCsv("a,b"));
        eq("StringEscapeUtils.escapeXml10", "&lt;x&gt;", StringEscapeUtils.escapeXml10("<x>"));
    }

    // =========================================================================
    private static void regexSection() {
        eq("RegExUtils.replaceAll", "a#b#c#", RegExUtils.replaceAll("a1b2c3", "[0-9]", "#"));
        eq("RegExUtils.removeAll", "abc", RegExUtils.removeAll("a1b2c3", "[0-9]"));
        eq("RegExUtils.replaceFirst", "a#b2", RegExUtils.replaceFirst("a1b2", "[0-9]", "#"));
        eq("RegExUtils.removeFirst", "ab2", RegExUtils.removeFirst("a1b2", "[0-9]"));
    }
}
