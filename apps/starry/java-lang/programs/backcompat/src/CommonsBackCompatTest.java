import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;

import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.IOException;
import java.io.InputStream;
import java.io.StringReader;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collection;
import java.util.List;
import java.util.Random;
import java.util.Set;
import java.util.TreeSet;

import org.apache.commons.collections4.Bag;
import org.apache.commons.collections4.BidiMap;
import org.apache.commons.collections4.CollectionUtils;
import org.apache.commons.collections4.MultiValuedMap;
import org.apache.commons.collections4.SortedBag;
import org.apache.commons.collections4.bag.HashBag;
import org.apache.commons.collections4.bag.TreeBag;
import org.apache.commons.collections4.bidimap.DualHashBidiMap;
import org.apache.commons.collections4.multimap.ArrayListValuedHashMap;
import org.apache.commons.io.FileUtils;
import org.apache.commons.io.FilenameUtils;
import org.apache.commons.io.IOUtils;
import org.apache.commons.lang3.ArrayUtils;
import org.apache.commons.lang3.BooleanUtils;
import org.apache.commons.lang3.RandomStringUtils;
import org.apache.commons.lang3.StringEscapeUtils;
import org.apache.commons.lang3.StringUtils;
import org.apache.commons.lang3.math.NumberUtils;
import org.apache.commons.lang3.mutable.MutableInt;
import org.apache.commons.lang3.text.WordUtils;
import org.apache.commons.lang3.tuple.ImmutablePair;
import org.apache.commons.lang3.tuple.Pair;
import org.apache.commons.lang3.tuple.Triple;
import org.apache.commons.math3.fraction.Fraction;
import org.apache.commons.math3.linear.Array2DRowRealMatrix;
import org.apache.commons.math3.linear.ArrayRealVector;
import org.apache.commons.math3.linear.DecompositionSolver;
import org.apache.commons.math3.linear.LUDecomposition;
import org.apache.commons.math3.linear.RealMatrix;
import org.apache.commons.math3.linear.RealVector;
import org.apache.commons.math3.stat.descriptive.DescriptiveStatistics;
import org.apache.commons.math3.util.ArithmeticUtils;
import org.apache.commons.math3.util.CombinatoricsUtils;
import org.junit.Test;

/**
 * Java-8 backward-compatibility carpet for the Apache Commons library group.
 *
 * Libraries under test (all Java-8-era, pure-Java):
 *   - commons-io           2.11.0
 *   - commons-math3        3.6.1
 *   - commons-lang3        3.12.0
 *   - commons-collections4 4.4
 *
 * Compiled with --release 8 (bytecode major 52). Uses ONLY Java 8 APIs so the
 * SAME .class runs identically on JDK 17/21/23/25. Every test uses fixed inputs
 * and asserts exact values (deterministic). RandomStringUtils uses a fixed-seed
 * java.util.Random so its output is reproducible across JVMs.
 */
public class CommonsBackCompatTest {

    // ============================================================
    // commons-io  (IOUtils / FileUtils / FilenameUtils)
    // ============================================================

    private static final byte[] FIXED_BYTES =
            "Hello, Commons-IO!\nLine2\nLine3".getBytes(StandardCharsets.UTF_8);

    @Test
    public void ioUtils_toByteArray_fromStream() throws IOException {
        InputStream in = new ByteArrayInputStream(FIXED_BYTES);
        byte[] out = IOUtils.toByteArray(in);
        assertArrayEquals(FIXED_BYTES, out);
        assertEquals(FIXED_BYTES.length, out.length);
    }

    @Test
    public void ioUtils_toString_charset() throws IOException {
        InputStream in = new ByteArrayInputStream(FIXED_BYTES);
        String s = IOUtils.toString(in, StandardCharsets.UTF_8);
        assertEquals("Hello, Commons-IO!\nLine2\nLine3", s);
    }

    @Test
    public void ioUtils_readLines() throws IOException {
        InputStream in = new ByteArrayInputStream(FIXED_BYTES);
        List<String> lines = IOUtils.readLines(in, StandardCharsets.UTF_8);
        assertEquals(3, lines.size());
        assertEquals("Hello, Commons-IO!", lines.get(0));
        assertEquals("Line2", lines.get(1));
        assertEquals("Line3", lines.get(2));
    }

    @Test
    public void ioUtils_copy_streamToStream() throws IOException {
        InputStream in = new ByteArrayInputStream(FIXED_BYTES);
        ByteArrayOutputStream bos = new ByteArrayOutputStream();
        int n = IOUtils.copy(in, bos);
        assertEquals(FIXED_BYTES.length, n);
        assertArrayEquals(FIXED_BYTES, bos.toByteArray());
    }

    @Test
    public void ioUtils_toCharArray() throws IOException {
        char[] chars = IOUtils.toCharArray(new StringReader("abcXYZ"));
        assertArrayEquals(new char[] { 'a', 'b', 'c', 'X', 'Y', 'Z' }, chars);
    }

    @Test
    public void ioUtils_contentEquals() throws IOException {
        InputStream a = new ByteArrayInputStream(FIXED_BYTES);
        InputStream b = new ByteArrayInputStream(FIXED_BYTES.clone());
        assertTrue(IOUtils.contentEquals(a, b));
        InputStream c = new ByteArrayInputStream(FIXED_BYTES);
        InputStream d = new ByteArrayInputStream("different".getBytes(StandardCharsets.UTF_8));
        assertFalse(IOUtils.contentEquals(c, d));
    }

    @Test
    public void fileUtils_writeRead_roundTrip() throws IOException {
        File tmp = File.createTempFile("commons-bc-", ".txt");
        tmp.deleteOnExit();
        try {
            FileUtils.writeByteArrayToFile(tmp, FIXED_BYTES);
            assertEquals(FIXED_BYTES.length, tmp.length());
            byte[] back = FileUtils.readFileToByteArray(tmp);
            assertArrayEquals(FIXED_BYTES, back);
            String str = FileUtils.readFileToString(tmp, StandardCharsets.UTF_8);
            assertEquals("Hello, Commons-IO!\nLine2\nLine3", str);
        } finally {
            FileUtils.deleteQuietly(tmp);
        }
        assertFalse(tmp.exists());
    }

    @Test
    public void fileUtils_writeLines_andReadLines() throws IOException {
        File tmp = File.createTempFile("commons-bc-lines-", ".txt");
        tmp.deleteOnExit();
        try {
            List<String> data = Arrays.asList("alpha", "beta", "gamma");
            FileUtils.writeLines(tmp, "UTF-8", data);
            List<String> back = FileUtils.readLines(tmp, StandardCharsets.UTF_8);
            assertEquals(data, back);
        } finally {
            FileUtils.deleteQuietly(tmp);
        }
    }

    @Test
    public void fileUtils_byteCountToDisplaySize() {
        assertEquals("1 KB", FileUtils.byteCountToDisplaySize(1024L));
        assertEquals("1 MB", FileUtils.byteCountToDisplaySize(1024L * 1024L));
        assertEquals("1 GB", FileUtils.byteCountToDisplaySize(1024L * 1024L * 1024L));
        assertEquals("512 bytes", FileUtils.byteCountToDisplaySize(512L));
    }

    @Test
    public void filenameUtils_pathManipulation() {
        assertEquals("txt", FilenameUtils.getExtension("/a/b/c/file.txt"));
        assertEquals("file", FilenameUtils.getBaseName("/a/b/c/file.txt"));
        assertEquals("file.txt", FilenameUtils.getName("/a/b/c/file.txt"));
        assertEquals("/a/b/c/", FilenameUtils.getFullPath("/a/b/c/file.txt"));
        assertEquals("/foo/baz.txt", FilenameUtils.normalize("/foo/bar/../baz.txt"));
        assertEquals("a/b/file", FilenameUtils.removeExtension("a/b/file.log"));
    }

    // ============================================================
    // commons-math3 (linear solve / stats / Fraction / combinatorics)
    // ============================================================

    @Test
    public void math3_solveLinearSystem() {
        // Solve:  2x + 3y = 8 ;  3x + 4y = 11  ->  x = 1, y = 2
        RealMatrix coeff = new Array2DRowRealMatrix(new double[][] {
                { 2.0, 3.0 },
                { 3.0, 4.0 }
        });
        DecompositionSolver solver = new LUDecomposition(coeff).getSolver();
        RealVector constants = new ArrayRealVector(new double[] { 8.0, 11.0 });
        RealVector solution = solver.solve(constants);
        assertEquals(1.0, solution.getEntry(0), 1e-9);
        assertEquals(2.0, solution.getEntry(1), 1e-9);
    }

    @Test
    public void math3_matrixDeterminantAndMultiply() {
        RealMatrix m = new Array2DRowRealMatrix(new double[][] {
                { 2.0, 3.0 },
                { 3.0, 4.0 }
        });
        // det = 2*4 - 3*3 = -1
        assertEquals(-1.0, new LUDecomposition(m).getDeterminant(), 1e-9);

        RealMatrix a = new Array2DRowRealMatrix(new double[][] { { 1, 2 }, { 3, 4 } });
        RealMatrix b = new Array2DRowRealMatrix(new double[][] { { 5, 6 }, { 7, 8 } });
        RealMatrix p = a.multiply(b); // [[19,22],[43,50]]
        assertEquals(19.0, p.getEntry(0, 0), 1e-9);
        assertEquals(22.0, p.getEntry(0, 1), 1e-9);
        assertEquals(43.0, p.getEntry(1, 0), 1e-9);
        assertEquals(50.0, p.getEntry(1, 1), 1e-9);
    }

    @Test
    public void math3_descriptiveStatistics() {
        double[] data = { 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0 };
        DescriptiveStatistics ds = new DescriptiveStatistics();
        for (double d : data) {
            ds.addValue(d);
        }
        assertEquals(10, ds.getN());
        assertEquals(5.5, ds.getMean(), 1e-9);
        assertEquals(1.0, ds.getMin(), 1e-9);
        assertEquals(10.0, ds.getMax(), 1e-9);
        assertEquals(55.0, ds.getSum(), 1e-9);
        // sample variance of 1..10 = 9.166666...
        assertEquals(9.166666666666666, ds.getVariance(), 1e-9);
        assertEquals(5.5, ds.getPercentile(50), 1e-9);
    }

    @Test
    public void math3_fractionArithmetic() {
        Fraction half = new Fraction(1, 2);
        Fraction third = new Fraction(1, 3);
        Fraction sum = half.add(third); // 5/6
        assertEquals(5, sum.getNumerator());
        assertEquals(6, sum.getDenominator());

        Fraction product = half.multiply(third); // 1/6
        assertEquals(1, product.getNumerator());
        assertEquals(6, product.getDenominator());

        Fraction diff = half.subtract(third); // 1/6
        assertEquals(1, diff.getNumerator());
        assertEquals(6, diff.getDenominator());

        Fraction reduced = Fraction.getReducedFraction(4, 8); // 1/2
        assertEquals(1, reduced.getNumerator());
        assertEquals(2, reduced.getDenominator());

        assertEquals(0.5, half.doubleValue(), 1e-12);
    }

    @Test
    public void math3_combinatorics() {
        assertEquals(120L, CombinatoricsUtils.factorial(5));
        assertEquals(10L, CombinatoricsUtils.binomialCoefficient(5, 2));
        assertEquals(252L, CombinatoricsUtils.binomialCoefficient(10, 5));
        assertEquals(1L, CombinatoricsUtils.factorial(0));
    }

    @Test
    public void math3_arithmeticUtils() {
        assertEquals(12, ArithmeticUtils.lcm(4, 6));
        assertEquals(6, ArithmeticUtils.gcd(12, 18));
        assertEquals(1024L, ArithmeticUtils.pow(2L, 10));
        assertTrue(ArithmeticUtils.isPowerOfTwo(64L));
        assertFalse(ArithmeticUtils.isPowerOfTwo(65L));
    }

    @Test
    public void math3_vectorOperations() {
        RealVector v1 = new ArrayRealVector(new double[] { 1.0, 2.0, 3.0 });
        RealVector v2 = new ArrayRealVector(new double[] { 4.0, 5.0, 6.0 });
        assertEquals(32.0, v1.dotProduct(v2), 1e-9); // 4+10+18
        RealVector sum = v1.add(v2);
        assertEquals(5.0, sum.getEntry(0), 1e-9);
        assertEquals(7.0, sum.getEntry(1), 1e-9);
        assertEquals(9.0, sum.getEntry(2), 1e-9);
        assertEquals(Math.sqrt(14.0), v1.getNorm(), 1e-9);
    }

    // ============================================================
    // commons-lang3 (StringUtils / WordUtils / Escape / ArrayUtils ...)
    // ============================================================

    @Test
    public void lang3_stringUtils_basics() {
        assertTrue(StringUtils.isBlank("   "));
        assertFalse(StringUtils.isBlank(" x "));
        assertTrue(StringUtils.isEmpty(""));
        assertEquals("abc", StringUtils.trimToEmpty("  abc  "));
        assertEquals("", StringUtils.trimToEmpty(null));
        assertEquals("abc", StringUtils.defaultString(null, "abc"));
        assertEquals("Hello", StringUtils.capitalize("hello"));
        assertEquals("hELLO", StringUtils.swapCase("Hello"));
        assertEquals("olleh", StringUtils.reverse("hello"));
    }

    @Test
    public void lang3_stringUtils_joinSplit() {
        assertEquals("a,b,c", StringUtils.join(new String[] { "a", "b", "c" }, ','));
        assertEquals("1-2-3", StringUtils.join(Arrays.asList(1, 2, 3), "-"));
        String[] parts = StringUtils.split("a,b,,c", ',');
        assertArrayEquals(new String[] { "a", "b", "c" }, parts);
        String[] kept = StringUtils.splitPreserveAllTokens("a,b,,c", ',');
        assertArrayEquals(new String[] { "a", "b", "", "c" }, kept);
    }

    @Test
    public void lang3_stringUtils_searchCount() {
        assertEquals(3, StringUtils.countMatches("ababab", "a"));
        assertEquals(2, StringUtils.countMatches("aaaa", "aa")); // non-overlapping
        assertTrue(StringUtils.containsIgnoreCase("Hello World", "WORLD"));
        assertEquals("xxbcd", StringUtils.replaceChars("aabcd", "a", "x"));
        assertEquals("hello world", StringUtils.replace("hello_world", "_", " "));
        assertEquals(4, StringUtils.indexOf("hello", "o"));
    }

    @Test
    public void lang3_stringUtils_padAbbreviate() {
        assertEquals("00042", StringUtils.leftPad("42", 5, '0'));
        assertEquals("42   ", StringUtils.rightPad("42", 5));
        assertEquals("  ab  ", StringUtils.center("ab", 6));
        assertEquals("abcdefg...", StringUtils.abbreviate("abcdefghijklmnop", 10));
        assertEquals("abc", StringUtils.repeat("abc", 1));
        assertEquals("xyxyxy", StringUtils.repeat("xy", 3));
    }

    @Test
    public void lang3_stringUtils_difference_levenshtein() {
        assertEquals(3, StringUtils.getLevenshteinDistance("kitten", "sitting"));
        assertEquals("You are funny", StringUtils.difference("I am hungry", "You are funny"));
        assertEquals("", StringUtils.difference("same", "same"));
    }

    @Test
    public void lang3_wordUtils() {
        assertEquals("I Am Fine", WordUtils.capitalize("i am fine"));
        assertEquals("I Am Fine", WordUtils.capitalizeFully("i AM fInE"));
        assertEquals("i am fine", WordUtils.uncapitalize("I Am Fine"));
        assertEquals("THE DOG", WordUtils.capitalize("the dog").toUpperCase());
        assertEquals("Hello\nWorld", WordUtils.wrap("Hello World", 5));
    }

    @Test
    public void lang3_stringEscapeUtils() {
        assertEquals("&lt;a&gt;&amp;&quot;", StringEscapeUtils.escapeHtml4("<a>&\""));
        assertEquals("<a>&\"", StringEscapeUtils.unescapeHtml4("&lt;a&gt;&amp;&quot;"));
        assertEquals("line1\\nline2", StringEscapeUtils.escapeJava("line1\nline2"));
        assertEquals("line1\nline2", StringEscapeUtils.unescapeJava("line1\\nline2"));
        assertEquals("&lt;tag&gt;", StringEscapeUtils.escapeXml10("<tag>"));
        assertEquals("\"a,b\"", StringEscapeUtils.escapeCsv("a,b"));
    }

    @Test
    public void lang3_arrayUtils() {
        int[] arr = { 1, 2, 3, 4, 5 };
        assertTrue(ArrayUtils.contains(arr, 3));
        assertFalse(ArrayUtils.contains(arr, 9));
        assertEquals(2, ArrayUtils.indexOf(arr, 3));
        int[] reversed = arr.clone();
        ArrayUtils.reverse(reversed);
        assertArrayEquals(new int[] { 5, 4, 3, 2, 1 }, reversed);
        int[] added = ArrayUtils.add(arr, 6);
        assertArrayEquals(new int[] { 1, 2, 3, 4, 5, 6 }, added);
        int[] removed = ArrayUtils.remove(arr, 0);
        assertArrayEquals(new int[] { 2, 3, 4, 5 }, removed);
        int[] sub = ArrayUtils.subarray(arr, 1, 4);
        assertArrayEquals(new int[] { 2, 3, 4 }, sub);
        assertTrue(ArrayUtils.isEmpty(new int[0]));
        assertEquals(5, ArrayUtils.getLength(arr));
    }

    @Test
    public void lang3_arrayUtils_toObjectAndPrimitive() {
        Integer[] boxed = ArrayUtils.toObject(new int[] { 1, 2, 3 });
        assertArrayEquals(new Integer[] { 1, 2, 3 }, boxed);
        int[] unboxed = ArrayUtils.toPrimitive(new Integer[] { 4, 5, 6 });
        assertArrayEquals(new int[] { 4, 5, 6 }, unboxed);
    }

    @Test
    public void lang3_mutable() {
        MutableInt mi = new MutableInt(10);
        mi.increment();
        assertEquals(11, mi.intValue());
        mi.add(4);
        assertEquals(15, mi.intValue());
        mi.subtract(5);
        assertEquals(10, mi.intValue());
        mi.setValue(100);
        assertEquals(100, mi.intValue());
    }

    @Test
    public void lang3_tuples() {
        Pair<String, Integer> p = ImmutablePair.of("key", 42);
        assertEquals("key", p.getLeft());
        assertEquals(Integer.valueOf(42), p.getRight());
        assertEquals("(key,42)", p.toString());

        Triple<String, Integer, Boolean> t = Triple.of("a", 1, true);
        assertEquals("a", t.getLeft());
        assertEquals(Integer.valueOf(1), t.getMiddle());
        assertEquals(Boolean.TRUE, t.getRight());
    }

    @Test
    public void lang3_numberUtils() {
        assertEquals(42, NumberUtils.toInt("42"));
        assertEquals(-1, NumberUtils.toInt("not-a-number", -1));
        assertEquals(3.14, NumberUtils.toDouble("3.14"), 1e-12);
        assertTrue(NumberUtils.isCreatable("123"));
        assertTrue(NumberUtils.isCreatable("0x1F"));
        assertFalse(NumberUtils.isCreatable("12.34.56"));
        assertEquals(9, NumberUtils.max(3, 9, 1));
        assertEquals(1, NumberUtils.min(3, 9, 1));
    }

    @Test
    public void lang3_booleanUtils() {
        assertTrue(BooleanUtils.toBoolean("yes"));
        assertTrue(BooleanUtils.toBoolean("true"));
        assertFalse(BooleanUtils.toBoolean("no"));
        assertEquals("yes", BooleanUtils.toStringYesNo(true));
        assertEquals("off", BooleanUtils.toStringOnOff(false));
        assertTrue(BooleanUtils.and(new boolean[] { true, true, true }));
        assertFalse(BooleanUtils.and(new boolean[] { true, false, true }));
        assertTrue(BooleanUtils.or(new boolean[] { false, false, true }));
    }

    @Test
    public void lang3_randomStringUtils_fixedSeedDeterministic() {
        // RandomStringUtils with an explicit fixed-seed Random => reproducible.
        Random rng1 = new Random(123456789L);
        String s1 = RandomStringUtils.random(20, 0, 0, true, true, null, rng1);
        Random rng2 = new Random(123456789L);
        String s2 = RandomStringUtils.random(20, 0, 0, true, true, null, rng2);
        assertEquals(s1, s2);
        assertEquals(20, s1.length());
        // Explicit fixed char-set over a fixed seed is reproducible across JVMs.
        Random rng3 = new Random(42L);
        char[] charset = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789".toCharArray();
        String a = RandomStringUtils.random(16, 0, charset.length, false, false, charset, rng3);
        Random rng4 = new Random(42L);
        String b = RandomStringUtils.random(16, 0, charset.length, false, false, charset, rng4);
        assertEquals(a, b);
        assertEquals(16, a.length());
        String universe = new String(charset);
        for (char c : a.toCharArray()) {
            assertTrue("char out of fixed set: " + c, universe.indexOf(c) >= 0);
        }
    }

    // ============================================================
    // commons-collections4 (Bag / BidiMap / MultiValuedMap / utils)
    // ============================================================

    @Test
    public void collections4_hashBag() {
        Bag<String> bag = new HashBag<String>();
        bag.add("apple", 3);
        bag.add("banana", 2);
        bag.add("apple"); // now 4 apples
        assertEquals(4, bag.getCount("apple"));
        assertEquals(2, bag.getCount("banana"));
        assertEquals(6, bag.size());
        assertEquals(0, bag.getCount("cherry"));
        Set<String> uniques = bag.uniqueSet();
        assertEquals(2, uniques.size());
        assertTrue(uniques.contains("apple"));
        assertTrue(uniques.contains("banana"));
        bag.remove("apple", 1);
        assertEquals(3, bag.getCount("apple"));
    }

    @Test
    public void collections4_treeBag_sorted() {
        SortedBag<String> bag = new TreeBag<String>();
        bag.add("gamma", 1);
        bag.add("alpha", 2);
        bag.add("beta", 3);
        assertEquals("alpha", bag.first());
        assertEquals("gamma", bag.last());
        assertEquals(2, bag.getCount("alpha"));
        assertEquals(6, bag.size());
        // uniqueSet of a TreeBag is sorted
        List<String> order = new ArrayList<String>(bag.uniqueSet());
        assertEquals(Arrays.asList("alpha", "beta", "gamma"), order);
    }

    @Test
    public void collections4_dualHashBidiMap() {
        BidiMap<String, Integer> map = new DualHashBidiMap<String, Integer>();
        map.put("one", 1);
        map.put("two", 2);
        map.put("three", 3);
        assertEquals(Integer.valueOf(2), map.get("two"));
        assertEquals("two", map.getKey(2));
        BidiMap<Integer, String> inverse = map.inverseBidiMap();
        assertEquals("three", inverse.get(3));
        // Re-putting an existing value re-maps uniquely (BidiMap invariant).
        map.put("uno", 1);
        assertNull(map.get("one"));
        assertEquals(Integer.valueOf(1), map.get("uno"));
        assertEquals("uno", map.getKey(1));
    }

    @Test
    public void collections4_multiValuedMap() {
        MultiValuedMap<String, Integer> mm = new ArrayListValuedHashMap<String, Integer>();
        mm.put("a", 1);
        mm.put("a", 2);
        mm.put("a", 3);
        mm.put("b", 10);
        Collection<Integer> aVals = mm.get("a");
        assertEquals(3, aVals.size());
        assertTrue(aVals.contains(1));
        assertTrue(aVals.contains(2));
        assertTrue(aVals.contains(3));
        assertEquals(1, mm.get("b").size());
        assertEquals(4, mm.size()); // total entries
        assertEquals(2, mm.keySet().size());
        assertTrue(mm.containsKey("a"));
        assertFalse(mm.containsKey("z"));
    }

    @Test
    public void collections4_collectionUtils_setOps() {
        List<Integer> a = Arrays.asList(1, 2, 3, 4);
        List<Integer> b = Arrays.asList(3, 4, 5, 6);
        Collection<Integer> union = CollectionUtils.union(a, b);
        Collection<Integer> inter = CollectionUtils.intersection(a, b);
        Collection<Integer> diff = CollectionUtils.subtract(a, b);

        TreeSet<Integer> unionSet = new TreeSet<Integer>(union);
        assertEquals(new TreeSet<Integer>(Arrays.asList(1, 2, 3, 4, 5, 6)), unionSet);

        TreeSet<Integer> interSet = new TreeSet<Integer>(inter);
        assertEquals(new TreeSet<Integer>(Arrays.asList(3, 4)), interSet);

        TreeSet<Integer> diffSet = new TreeSet<Integer>(diff);
        assertEquals(new TreeSet<Integer>(Arrays.asList(1, 2)), diffSet);
    }

    @Test
    public void collections4_collectionUtils_predicatesAndMisc() {
        List<Integer> nums = Arrays.asList(2, 4, 6, 8);
        boolean allEven = CollectionUtils.matchesAll(nums, n -> n % 2 == 0);
        assertTrue(allEven);
        assertEquals(4, CollectionUtils.countMatches(nums, n -> n > 0));
        assertEquals(2, CollectionUtils.countMatches(nums, n -> n > 4));
        assertTrue(CollectionUtils.isEqualCollection(
                Arrays.asList(1, 2, 3), Arrays.asList(3, 2, 1)));
        assertFalse(CollectionUtils.isEmpty(nums));
        assertTrue(CollectionUtils.isEmpty(new ArrayList<Integer>()));
        Object item = CollectionUtils.get(nums, 3);
        assertEquals(Integer.valueOf(8), item);
    }

    @Test
    public void collections4_collectionUtils_transformSelect() {
        List<Integer> src = Arrays.asList(1, 2, 3, 4, 5);
        Collection<Integer> doubled = CollectionUtils.collect(src, n -> n * 2);
        assertEquals(Arrays.asList(2, 4, 6, 8, 10), new ArrayList<Integer>(doubled));
        Collection<Integer> selected = CollectionUtils.select(src, n -> n % 2 == 1);
        assertEquals(new TreeSet<Integer>(Arrays.asList(1, 3, 5)),
                new TreeSet<Integer>(selected));
        assertNotNull(CollectionUtils.find(src, n -> n == 3));
        assertNull(CollectionUtils.find(src, n -> n == 99));
    }
}
