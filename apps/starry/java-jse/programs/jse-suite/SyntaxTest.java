import java.lang.annotation.*;
import java.util.*;
import java.util.function.*;

/* Carpet-grade coverage of the Java 17 language: syntax, operators, control
 * flow, the type system and every standard (non-preview) language feature.
 *
 * Every assertion checks an exact deterministic value (==, equals or a known
 * constant). No external I/O, no network, no JIT/timing dependence; pure
 * in-heap computation plus in-process annotation reflection. musl / StarryOS
 * safe.
 *
 * Coverage matrix:
 *   primitives/literals  binary/octal/hex, underscores, L/f/d suffixes, char
 *                        escapes, unicode escapes, char arithmetic, narrowing
 *   operators            arithmetic, integer div/modulo (neg), bitwise &|^~,
 *                        shifts <<,>>,>>> + masking, compound assign, ++/--,
 *                        ternary + numeric promotion, short/non-short-circuit
 *   numeric edge         overflow wrap, addExact, NaN, +/-Infinity, -0.0,
 *                        Double.compare, float rounding, div-by-zero
 *   control flow         if/while/do-while/for, switch stmt fallthrough,
 *                        switch expression (arrow/yield/multi-label/String/enum),
 *                        labeled break/continue, enhanced-for
 *   arrays               1D/2D/3D, jagged, initializer, clone, defaults,
 *                        covariance + ArrayStoreException
 *   strings/text blocks  text block stripping/continuation/\s, repeat/strip/lines
 *   classes/objects      static/inner/local/anonymous classes, qualified this,
 *                        constructor chaining, field hiding + super, overriding,
 *                        dynamic dispatch, covariant return, overload resolution
 *   interfaces           default/static/private methods, diamond super,
 *                        constant fields, functional interface
 *   generics             generic class/method, bounded + multiple bounds,
 *                        wildcards ? extends/? super (PECS)/unbounded, inference
 *   records              canonical/compact/custom ctor, accessors, equals/
 *                        hashCode/toString, generic record, sealed record tree
 *   sealed types         sealed/permits/non-sealed, subclassing
 *   enums                values/valueOf/ordinal/name/compareTo, fields+abstract
 *                        methods, EnumSet/EnumMap, exhaustive enum switch
 *   pattern matching     instanceof type patterns, flow scoping, negation scope
 *   lambdas/method refs  4 method-ref kinds, array-ctor ref, var params,
 *                        capture, Function/Predicate/Bi-x/UnaryOp/BinaryOp combos
 *   exceptions           try/catch/finally, multi-catch, try-with-resources
 *                        (single/LIFO/suppressed), chaining, finally-overrides
 *   varargs              counts, array pass-through, @SafeVarargs generic
 *   autoboxing           Integer cache (-128..127), collection box/unbox, NPE
 *   var / annotations    local var inference, runtime annotation reflection
 */
public class SyntaxTest {
    static int ok = 0, fail = 0;
    static void check(boolean c, String name) {
        if (c) { ok++; } else { fail++; System.out.println("FAIL " + name); }
    }

    // ------------------------------------------------------------------
    // records + sealed type hierarchy
    // ------------------------------------------------------------------
    record Point(int x, int y) {}
    record Pair<A, B>(A a, B b) { Pair { Objects.requireNonNull(a, "a"); } }
    record Range(int lo, int hi) {
        Range { if (lo > hi) throw new IllegalArgumentException("lo>hi"); }
        int span() { return hi - lo; }
        static Range unit() { return new Range(0, 1); }
    }

    sealed interface Shape permits Circle, Square, Rect {}
    record Circle(double r) implements Shape {}
    record Square(double s) implements Shape {}
    static final class Rect implements Shape {
        final double w, h;
        Rect(double w, double h) { this.w = w; this.h = h; }
    }
    static double area(Shape sh) {
        if (sh instanceof Circle c) return Math.PI * c.r() * c.r();
        if (sh instanceof Square sq) return sq.s() * sq.s();
        if (sh instanceof Rect r) return r.w * r.h;
        return 0;
    }

    sealed interface Expr permits Num, Add, Mul {}
    record Num(int v) implements Expr {}
    record Add(Expr l, Expr r) implements Expr {}
    record Mul(Expr l, Expr r) implements Expr {}
    static int eval(Expr e) {
        if (e instanceof Num n) return n.v();
        if (e instanceof Add a) return eval(a.l()) + eval(a.r());
        if (e instanceof Mul m) return eval(m.l()) * eval(m.r());
        throw new IllegalStateException();
    }

    sealed interface Vehicle permits Car, Truck, Bike {}
    static final class Car implements Vehicle {}
    static non-sealed class Truck implements Vehicle {}
    static final class Bike implements Vehicle {}
    static final class BigTruck extends Truck {}

    // ------------------------------------------------------------------
    // enums
    // ------------------------------------------------------------------
    enum Op {
        ADD("+") { int ap(int a, int b) { return a + b; } },
        SUB("-") { int ap(int a, int b) { return a - b; } },
        MUL("*") { int ap(int a, int b) { return a * b; } };
        final String sym;
        Op(String s) { this.sym = s; }
        abstract int ap(int a, int b);
    }
    enum Day { MON, TUE, WED, THU, FRI, SAT, SUN }

    // ------------------------------------------------------------------
    // interfaces (default / static / private / diamond / constant)
    // ------------------------------------------------------------------
    interface Greeter {
        String name();
        default String greet() { return prefix() + name(); }
        private String prefix() { return "Hi, "; }
        static Greeter of(String n) { return () -> n; }
    }
    interface A1 { default String who() { return "A1"; } }
    interface B1 { default String who() { return "B1"; } }
    static final class C1 implements A1, B1 {
        public String who() { return A1.super.who() + B1.super.who(); }
    }
    interface Const { int X = 42; }
    @FunctionalInterface interface TriFunction<A, B, C, R> { R apply(A a, B b, C c); }

    // ------------------------------------------------------------------
    // generic class
    // ------------------------------------------------------------------
    static final class Box<T> {
        private final T val;
        Box(T v) { val = v; }
        T get() { return val; }
        <R> Box<R> map(Function<? super T, ? extends R> f) { return new Box<>(f.apply(val)); }
    }

    // ------------------------------------------------------------------
    // nested / inner classes
    // ------------------------------------------------------------------
    static class Outer {
        int v = 10;
        static int sv = 99;
        class Inner {
            int x = 2;
            int outerV() { return v; }
            int qualifiedThis() { return Outer.this.v + this.x; }
        }
        static class Nested { int get() { return sv; } }
    }
    static class Base { int v = 1; String tag() { return "B"; } }
    static class Sub extends Base {
        int v = 2;
        String describe() { return "base=" + super.v + ",sub=" + this.v; }
        @Override String tag() { return "S"; }
    }
    static class Base2 { Base2 self() { return this; } }
    static class Sub2 extends Base2 { @Override Sub2 self() { return this; } }
    static class Chained {
        int sum;
        Chained() { this(6); }
        Chained(int s) { this.sum = s; }
    }

    // ------------------------------------------------------------------
    // custom iterable, exceptions, resources
    // ------------------------------------------------------------------
    static final class Countdown implements Iterable<Integer> {
        final int from;
        Countdown(int from) { this.from = from; }
        public Iterator<Integer> iterator() {
            return new Iterator<>() {
                int cur = from;
                public boolean hasNext() { return cur > 0; }
                public Integer next() { return cur--; }
            };
        }
    }
    static class AppException extends Exception {
        final int code;
        AppException(String m, int code) { super(m); this.code = code; }
        AppException(String m, Throwable c) { super(m, c); this.code = -1; }
    }
    static class Res implements AutoCloseable {
        static boolean closed = false;
        public void close() { closed = true; }
    }
    static final List<String> closeOrder = new ArrayList<>();
    static final class Track implements AutoCloseable {
        final String id;
        Track(String id) { this.id = id; }
        public void close() { closeOrder.add(id); }
    }
    static final class FailClose implements AutoCloseable {
        public void close() { throw new RuntimeException("close-fail"); }
    }

    // ------------------------------------------------------------------
    // annotations
    // ------------------------------------------------------------------
    @Retention(RetentionPolicy.RUNTIME)
    @interface Tag { String value(); int n() default 7; String[] tags() default {}; }
    @Retention(RetentionPolicy.RUNTIME)
    @interface Marker {}
    @Tag(value = "demo", n = 3, tags = {"x", "y"})
    static final class Tagged {}
    @Marker static final class Marked {}

    // ------------------------------------------------------------------
    // method helpers (overloading, varargs, generics, control flow)
    // ------------------------------------------------------------------
    static String over(int x) { return "int"; }
    static String over(long x) { return "long"; }
    static String over(double x) { return "double"; }
    static String over(Integer x) { return "Integer"; }
    static String over(Object x) { return "Object"; }

    static int vlen(String... xs) { return xs.length; }
    @SafeVarargs static <T> List<T> listOf(T... xs) { return new ArrayList<>(Arrays.asList(xs)); }

    static <T extends Comparable<T>> T maxOf(T a, T b) { return a.compareTo(b) >= 0 ? a : b; }
    static double sumNum(List<? extends Number> xs) { double s = 0; for (Number n : xs) s += n.doubleValue(); return s; }
    static <T> void copy(List<? extends T> src, List<? super T> dst) { for (T t : src) dst.add(t); }
    static <T> T firstOrDefault(List<T> xs, T def) { return xs.isEmpty() ? def : xs.get(0); }
    static <T extends Number & Comparable<T>> T clampMin(T v, T min) { return v.compareTo(min) < 0 ? min : v; }

    static int finallyWins() { try { return 1; } finally { return 2; } }
    static void chain() throws AppException {
        try { throw new IllegalStateException("root"); }
        catch (IllegalStateException e) { throw new AppException("wrapped", e); }
    }
    static String classify(RuntimeException e) {
        try { throw e; }
        catch (NumberFormatException | ArrayIndexOutOfBoundsException x) { return "num-or-arr"; }
        catch (IllegalStateException x) { return "state"; }
        catch (RuntimeException x) { return "other"; }
    }
    static String describe(Object o) {
        if (o instanceof String s) return "str:" + s.length();
        if (o instanceof Integer i && i > 0) return "int:" + i;
        return "other";
    }
    static String firstChar(Object o) {
        if (!(o instanceof String s)) return "none";
        return String.valueOf(s.charAt(0));
    }
    static boolean tick(int[] c) { c[0]++; return true; }

    // ==================================================================
    public static void main(String[] args) throws Exception {
        primitivesAndLiterals();
        operators();
        numericEdge();
        controlFlow();
        arrays();
        stringsAndTextBlocks();
        classesAndObjects();
        interfaces();
        generics();
        records();
        sealedTypes();
        enums();
        patternMatching();
        lambdasAndMethodRefs();
        exceptions();
        varargsAndAutobox();
        varAndAnnotations();

        System.out.println("SYNTAX_RESULT ok=" + ok + " fail=" + fail);
        if (fail == 0) System.out.println("SYNTAX_DONE");
    }

    // ------------------------------------------------------------------
    static void primitivesAndLiterals() {
        check(0b1010 == 10 && 0xFF == 255 && 010 == 8 && 1_000_000 == 1000000, "numeric-literals");
        check(10_000_000_000L == 10000000000L && 1.5f == 1.5 && 1.5d == 1.5, "literal-suffixes");
        check('A' == 'A' && '\n' == 10 && '\t' == 9 && '\\' == 92, "char-escapes-unicode");
        check((char) ('A' + 1) == 'B' && (char) ('Z' - 25) == 'A' && 'A' + 1 == 66, "char-arithmetic");
        check(Character.isDigit('7') && Character.getNumericValue('7') == 7 && Character.toUpperCase('a') == 'A', "character-methods");
        // widening / narrowing
        long wl = Integer.MAX_VALUE; wl += 1;
        check(wl == 2147483648L, "widening-int-to-long");
        check((int) 3.99 == 3 && (byte) 257 == 1 && (int) 'A' == 65 && (short) 70000 == 4464, "narrowing-cast");
        check((int) Float.NaN == 0 && (int) Double.POSITIVE_INFINITY == Integer.MAX_VALUE, "cast-nan-inf");
        boolean bt = true, bf = false;
        check(bt && !bf && (bt ^ bf), "boolean-primitive");
    }

    // ------------------------------------------------------------------
    static void operators() {
        check((0b1100 & 0b1010) == 0b1000 && (0b1100 | 0b1010) == 0b1110
                && (0b1100 ^ 0b1010) == 0b0110 && (~0) == -1, "bitwise-ops");
        check((1 << 31) == Integer.MIN_VALUE && (-8 >> 1) == -4 && (-8 >>> 28) == 15, "shift-ops");
        check((1 << 32) == 1 && (1L << 63) == Long.MIN_VALUE && (1 << 33) == 2, "shift-distance-masking");
        check(-7 % 3 == -1 && 7 % -3 == 1 && -7 / 2 == -3 && 7 / 2 == 3, "integer-div-modulo");
        check(Math.floorMod(-7, 3) == 2 && Math.floorDiv(-7, 3) == -3, "floor-div-mod");
        // increment / decrement
        int i = 5; check(i++ == 5 && i == 6, "post-increment");
        int j = 5; check(++j == 6 && j == 6, "pre-increment");
        int k = 5; int res = k++ + ++k; check(res == 12 && k == 7, "mixed-inc-dec");
        // compound assignment with implicit narrowing
        byte bb = 10; bb += 5; check(bb == 15, "compound-narrowing");
        int sh = 1; sh <<= 4; check(sh == 16, "compound-shift");
        int acc = 10; acc *= 3; acc -= 5; acc /= 5; check(acc == 5, "compound-chain");
        // ternary numeric promotion -> double
        Object t = true ? 1 : 2.0;
        check(t instanceof Double && (Double) t == 1.0, "ternary-numeric-promotion");
        // string concatenation evaluation order + types
        check(("" + 1 + 2).equals("12") && (1 + 2 + "").equals("3"), "concat-eval-order");
        check(('a' + 'b') == 195 && ("" + 'a' + 'b').equals("ab"), "char-vs-string-concat");
        // short-circuit
        int[] sc = {0};
        boolean r1 = false || tick(sc);
        boolean r2 = true || tick(sc);
        check(sc[0] == 1 && r1 && r2, "short-circuit-or");
        sc[0] = 0;
        boolean r3 = true && tick(sc);
        boolean r4 = false && tick(sc);
        check(sc[0] == 1 && r3 && !r4, "short-circuit-and");
        // non-short-circuit & evaluates both
        int[] cc = {0};
        boolean nb = tick(cc) & tick(cc);
        check(cc[0] == 2 && nb, "non-shortcircuit-and");
    }

    // ------------------------------------------------------------------
    static void numericEdge() {
        check(Integer.MAX_VALUE + 1 == Integer.MIN_VALUE && Long.MIN_VALUE - 1 == Long.MAX_VALUE, "overflow-wrap");
        boolean ovf = false;
        try { Math.addExact(Integer.MAX_VALUE, 1); } catch (ArithmeticException e) { ovf = true; }
        check(ovf, "addexact-overflow-throws");
        check(Double.NaN != Double.NaN && Double.isNaN(0.0 / 0.0), "nan-not-self-equal");
        check(1.0 / 0.0 == Double.POSITIVE_INFINITY && -1.0 / 0.0 == Double.NEGATIVE_INFINITY, "double-infinity");
        check(Double.compare(Double.NaN, Double.NaN) == 0 && Double.compare(0.0, -0.0) > 0, "double-compare-nan-zero");
        check(0.0 == -0.0 && (1.0 / 0.0) != (1.0 / -0.0), "signed-zero");
        check(0.1 + 0.2 != 0.3, "float-rounding-error");
        // int division by zero throws; "".length() is not a constant expression
        boolean divZero = false;
        try { int z = 1 / "".length(); divZero = (z == 99); } catch (ArithmeticException e) { divZero = true; }
        check(divZero, "int-div-by-zero-throws");
    }

    // ------------------------------------------------------------------
    static void controlFlow() {
        // switch statement fallthrough (start case 3 -> ++ at 3,4,5)
        int fc = 0;
        switch (3) {
            case 1: fc++;
            case 2: fc++;
            case 3: fc++;
            case 4: fc++;
            case 5: fc++; break;
            default: fc = -1;
        }
        check(fc == 3, "switch-fallthrough");
        // switch expression arrow + multi-label
        String sz = switch (4) {
            case 1, 2, 3, 4, 5 -> "low";
            case 6, 7, 8, 9 -> "high";
            default -> "other";
        };
        check(sz.equals("low"), "switch-expr-arrow-multilabel");
        // switch expression yield (block body)
        int sv = switch (2) {
            case 1 -> 10;
            case 2 -> { int tmp = 20; yield tmp + 5; }
            default -> 0;
        };
        check(sv == 25, "switch-expr-yield");
        // switch on String
        String ss = switch ("b") { case "a" -> "A"; case "b" -> "B"; default -> "?"; };
        check(ss.equals("B"), "switch-on-string");
        // labeled break out of nested loops
        int found = -1;
        outer:
        for (int a = 0; a < 3; a++)
            for (int b = 0; b < 3; b++)
                if (a * 3 + b == 5) { found = a * 10 + b; break outer; }
        check(found == 12, "labeled-break");
        // labeled continue
        int lcSum = 0;
        out:
        for (int a = 0; a < 3; a++)
            for (int b = 0; b < 3; b++) {
                if (b == 1) continue out;
                lcSum += a * 10 + b;
            }
        check(lcSum == 30, "labeled-continue");
        // do-while runs at least once
        int iters = 0, n = 0;
        do { iters++; } while (n > 0);
        check(iters == 1, "do-while-runs-once");
        // while + for accumulation
        int wsum = 0, w = 0; while (w < 5) { wsum += w; w++; }
        int fsum = 0; for (int x = 0; x < 5; x++) fsum += x;
        check(wsum == 10 && fsum == 10, "while-for-accumulate");
        // enhanced-for over array and over custom Iterable
        int ef = 0; for (int x : new int[]{1, 2, 3, 4}) ef += x;
        int cd = 0; for (int x : new Countdown(4)) cd += x;
        check(ef == 10 && cd == 10, "enhanced-for-array-iterable");
    }

    // ------------------------------------------------------------------
    static void arrays() {
        int[][] grid = new int[3][4];
        check(grid.length == 3 && grid[0].length == 4 && grid[2][3] == 0, "array-2d-defaults");
        int[][] jagged = {{1}, {2, 3}, {4, 5, 6}};
        check(jagged[2].length == 3 && jagged[1][1] == 3 && jagged[0].length == 1, "jagged-array");
        int[][][] cube = new int[2][2][2];
        cube[1][1][1] = 7;
        check(cube[1][1][1] == 7 && cube[0][0][0] == 0, "array-3d");
        int[] a = {5, 3, 1, 4, 2};
        int[] b = a.clone();
        b[0] = 99;
        check(a[0] == 5 && b[0] == 99 && a.length == 5, "array-clone-independent");
        String[] strs = new String[2];
        check(strs[0] == null && strs.length == 2, "array-ref-defaults");
        // array covariance + ArrayStoreException
        Object[] cov = new String[2];
        cov[0] = "ok";
        boolean ase = false;
        try { cov[1] = Integer.valueOf(1); } catch (ArrayStoreException e) { ase = true; }
        check(cov[0].equals("ok") && ase, "array-store-exception");
        // out of bounds
        boolean oob = false;
        try { int x = a[10]; oob = (x == -1); } catch (ArrayIndexOutOfBoundsException e) { oob = true; }
        check(oob, "array-index-oob");
    }

    // ------------------------------------------------------------------
    static void stringsAndTextBlocks() {
        // no trailing newline: closing delimiter on the content line
        String tb1 = """
                {"k":1}""";
        check(tb1.equals("{\"k\":1}"), "textblock-no-trailing-newline");
        // multi-line with incidental whitespace stripping (closing on own line -> trailing \n)
        String tb2 = """
                Hello
                World
                """;
        check(tb2.equals("Hello\nWorld\n"), "textblock-multiline-strip");
        // line continuation: trailing backslash removes the line break
        String tb3 = """
                a\
                b""";
        check(tb3.equals("ab"), "textblock-line-continuation");
        // \s space escape preserves a trailing space
        String tb4 = """
                x\s
                y""";
        check(tb4.equals("x \ny"), "textblock-space-escape");
        check(tb2.lines().count() == 2, "textblock-lines-count");
        // language-relevant string operations
        check("ab".repeat(3).equals("ababab") && "  x  ".strip().equals("x"), "string-repeat-strip");
        check("a\nb\nc".lines().count() == 3 && "Hello".chars().sum() == 500, "string-lines-chars");
    }

    // ------------------------------------------------------------------
    static void classesAndObjects() {
        Outer o = new Outer();
        Outer.Inner in = o.new Inner();
        check(in.outerV() == 10 && in.qualifiedThis() == 12, "inner-class-qualified-this");
        check(new Outer.Nested().get() == 99, "static-nested-class");
        // anonymous class
        Greeter anon = new Greeter() { public String name() { return "Anon"; } };
        check(anon.greet().equals("Hi, Anon"), "anonymous-class");
        // local class
        class Local { int twice(int x) { return x * 2; } }
        check(new Local().twice(21) == 42, "local-class");
        // local class capturing an effectively-final local
        int cap = 7;
        class Adder { int add(int x) { return x + cap; } }
        check(new Adder().add(3) == 10, "local-class-capture");
        // anonymous capturing + mutating a captured array
        final int[] counter = {0};
        Runnable r = new Runnable() { public void run() { counter[0]++; } };
        r.run(); r.run();
        check(counter[0] == 2, "anonymous-capture-mutate");
        // constructor chaining via this(...)
        check(new Chained().sum == 6 && new Chained(10).sum == 10, "constructor-chaining");
        // field hiding + super field access
        check(new Sub().describe().equals("base=1,sub=2"), "field-hiding-super");
        // dynamic dispatch: method virtual, field static-type bound
        Base bp = new Sub();
        check(bp.tag().equals("S") && bp.v == 1, "dispatch-method-vs-field");
        // covariant return type
        check(new Sub2().self() instanceof Sub2, "covariant-return");
        // overload resolution
        check(over(5).equals("int") && over(5L).equals("long") && over(5.0).equals("double")
                && over("s").equals("Object") && over(Integer.valueOf(9)).equals("Integer"), "overload-resolution");
        // downcast + ClassCastException
        Object str = "str";
        boolean cce = false;
        try { Integer bad = (Integer) str; cce = (bad == null); } catch (ClassCastException e) { cce = true; }
        check(cce, "class-cast-exception");
    }

    // ------------------------------------------------------------------
    static void interfaces() {
        Greeter g = Greeter.of("Bob");
        check(g.greet().equals("Hi, Bob"), "interface-default-private-static");
        check(new C1().who().equals("A1B1"), "interface-diamond-super");
        check(Const.X == 42, "interface-constant-field");
        TriFunction<Integer, Integer, Integer, Integer> tri = (x, y, z) -> x + y + z;
        check(tri.apply(1, 2, 3) == 6, "custom-functional-interface");
    }

    // ------------------------------------------------------------------
    static void generics() {
        Box<Integer> bi = new Box<>(21);
        Box<String> bs = bi.map(x -> "v" + (x * 2));
        check(bi.get() == 21 && bs.get().equals("v42"), "generic-class-map");
        check(maxOf(3, 7) == 7 && maxOf("apple", "banana").equals("banana"), "generic-bounded-method");
        check(clampMin(3, 5) == 5 && clampMin(8, 5) == 8, "generic-multiple-bounds");
        check(sumNum(List.of(1, 2, 3)) == 6.0 && sumNum(List.of(1.5, 2.5)) == 4.0, "wildcard-extends");
        List<Object> dst = new ArrayList<>();
        copy(List.of("a", "b"), dst);
        check(dst.size() == 2 && dst.get(0).equals("a"), "wildcard-super-pecs");
        List<?> any = List.of(1, 2, 3, 4);
        check(any.size() == 4, "wildcard-unbounded");
        check(firstOrDefault(List.of(9, 8), -1) == 9 && firstOrDefault(List.<Integer>of(), -1) == -1, "generic-method-inference");
        // diamond on nested generic
        Map<String, List<Integer>> mm = new HashMap<>();
        mm.computeIfAbsent("k", x -> new ArrayList<>()).add(7);
        check(mm.get("k").get(0) == 7, "diamond-nested-generic");
    }

    // ------------------------------------------------------------------
    static void records() {
        Point p = new Point(3, 4);
        check(p.x() == 3 && p.y() == 4, "record-accessors");
        check(p.equals(new Point(3, 4)) && !p.equals(new Point(3, 5)), "record-equals");
        check(p.hashCode() == new Point(3, 4).hashCode(), "record-hashcode-consistent");
        check(p.toString().equals("Point[x=3, y=4]"), "record-tostring");
        Pair<String, Integer> pr = new Pair<>("k", 9);
        check(pr.a().equals("k") && pr.b() == 9, "record-generic");
        boolean npe = false;
        try { new Pair<>(null, 1); } catch (NullPointerException e) { npe = true; }
        check(npe, "record-compact-ctor-validation");
        Range rg = new Range(2, 5);
        check(rg.span() == 3 && Range.unit().span() == 1, "record-instance-static-methods");
        boolean iae = false;
        try { new Range(5, 2); } catch (IllegalArgumentException e) { iae = true; }
        check(iae, "record-ctor-throws");
    }

    // ------------------------------------------------------------------
    static void sealedTypes() {
        check(Math.abs(area(new Circle(2)) - Math.PI * 4) < 1e-9, "sealed-circle-area");
        check(area(new Square(3)) == 9.0 && area(new Rect(2, 3)) == 6.0, "sealed-square-rect-area");
        // sealed expression tree + recursion + instanceof
        Expr e = new Add(new Num(3), new Mul(new Num(4), new Num(5)));
        check(eval(e) == 23, "sealed-expr-eval");
        // non-sealed permits further subclassing
        Vehicle v = new BigTruck();
        check(v instanceof Truck && v instanceof Vehicle && new Car() instanceof Vehicle, "sealed-nonsealed-hierarchy");
    }

    // ------------------------------------------------------------------
    static void enums() {
        check(Op.ADD.ap(6, 4) == 10 && Op.SUB.ap(6, 4) == 2 && Op.MUL.ap(6, 4) == 24, "enum-abstract-methods");
        check(Op.ADD.sym.equals("+") && Op.MUL.sym.equals("*"), "enum-instance-field");
        check(Op.values().length == 3 && Op.valueOf("MUL") == Op.MUL, "enum-values-valueof");
        check(Op.ADD.ordinal() == 0 && Op.MUL.ordinal() == 2 && Op.SUB.name().equals("SUB"), "enum-ordinal-name");
        check(Day.MON.compareTo(Day.WED) < 0 && Day.SUN.compareTo(Day.MON) > 0, "enum-compareto");
        EnumSet<Day> wd = EnumSet.range(Day.MON, Day.FRI);
        check(wd.size() == 5 && wd.contains(Day.WED) && !wd.contains(Day.SAT), "enumset-range");
        EnumMap<Day, String> em = new EnumMap<>(Day.class);
        em.put(Day.MON, "work");
        check(em.get(Day.MON).equals("work") && em.size() == 1, "enummap");
        // exhaustive enum switch expression (no default needed)
        String cat = switch (Day.SUN) {
            case SAT, SUN -> "weekend";
            case MON, TUE, WED, THU, FRI -> "weekday";
        };
        check(cat.equals("weekend"), "enum-switch-exhaustive");
        boolean ive = false;
        try { Op.valueOf("NOPE"); } catch (IllegalArgumentException e) { ive = true; }
        check(ive, "enum-valueof-invalid");
    }

    // ------------------------------------------------------------------
    static void patternMatching() {
        check(describe("pattern").equals("str:7"), "instanceof-pattern-string");
        check(describe(42).equals("int:42"), "instanceof-pattern-and-guard");
        check(describe(3.5).equals("other"), "instanceof-pattern-fallthrough");
        check(firstChar("Xyz").equals("X") && firstChar(5).equals("none"), "instanceof-negation-scope");
        // pattern binding usable in the conjunction
        Object obj = "hello";
        boolean both = obj instanceof String s && s.length() == 5;
        check(both, "instanceof-binding-in-condition");
    }

    // ------------------------------------------------------------------
    static void lambdasAndMethodRefs() {
        Function<Integer, Integer> sq = x -> x * x;
        Function<Integer, Integer> inc = x -> x + 1;
        check(sq.andThen(inc).apply(3) == 10 && sq.compose(inc).apply(3) == 16, "function-compose-andthen");
        Predicate<Integer> even = x -> x % 2 == 0;
        Predicate<Integer> pos = x -> x > 0;
        check(even.and(pos).test(4) && !even.and(pos).test(-4) && even.or(pos).test(3) && even.negate().test(3), "predicate-combinators");
        BinaryOperator<Integer> mul = (x, y) -> x * y;
        UnaryOperator<String> up = s -> s.toUpperCase();
        check(mul.apply(6, 7) == 42 && up.apply("hi").equals("HI"), "binary-unary-operator");
        Supplier<String> sup = () -> "S";
        Consumer<int[]> cons = arr -> arr[0] = 5;
        int[] holder = new int[1]; cons.accept(holder);
        check(sup.get().equals("S") && holder[0] == 5, "supplier-consumer");
        // 4 method-reference kinds + array constructor reference
        BiFunction<Integer, Integer, Integer> add = Integer::sum;        // static
        String hello = "Hello";
        Supplier<Integer> blen = hello::length;                          // bound instance
        Function<String, Integer> ulen = String::length;                 // unbound instance
        Supplier<List<String>> ctor = ArrayList::new;                    // constructor
        IntFunction<int[]> arrCtor = int[]::new;                         // array constructor
        check(add.apply(3, 4) == 7 && blen.get() == 5 && ulen.apply("abcd") == 4
                && ctor.get().isEmpty() && arrCtor.apply(5).length == 5, "method-refs-all-kinds");
        // var lambda parameters (Java 11+)
        BiFunction<Integer, Integer, Integer> vp = (var x, var y) -> x + y;
        check(vp.apply(2, 3) == 5, "lambda-var-params");
        // capture of effectively-final local
        int base = 100;
        Function<Integer, Integer> addBase = x -> x + base;
        check(addBase.apply(5) == 105, "lambda-capture");
    }

    // ------------------------------------------------------------------
    static void exceptions() {
        // try-with-resources single
        Res.closed = false;
        try (Res r = new Res()) { /* use */ }
        check(Res.closed, "twr-single-close");
        // multiple resources close in LIFO order
        closeOrder.clear();
        try (Track a = new Track("A"); Track b = new Track("B"); Track c = new Track("C")) { /* use */ }
        check(closeOrder.equals(List.of("C", "B", "A")), "twr-lifo-order");
        // suppressed exception
        boolean okSup = false;
        try (FailClose f = new FailClose()) {
            throw new IllegalStateException("body");
        } catch (Exception ex) {
            Throwable[] sup = ex.getSuppressed();
            okSup = ex.getMessage().equals("body") && sup.length == 1 && sup[0].getMessage().equals("close-fail");
        }
        check(okSup, "twr-suppressed-exception");
        // multi-catch dispatch
        check(classify(new NumberFormatException()).equals("num-or-arr")
                && classify(new ArrayIndexOutOfBoundsException()).equals("num-or-arr")
                && classify(new IllegalStateException()).equals("state")
                && classify(new RuntimeException()).equals("other"), "multi-catch-dispatch");
        // finally overrides return value
        check(finallyWins() == 2, "finally-overrides-return");
        // exception chaining via getCause
        boolean chained = false;
        try { chain(); }
        catch (AppException ae) { chained = ae.getCause() instanceof IllegalStateException && ae.getCause().getMessage().equals("root"); }
        check(chained, "exception-chaining-cause");
        // custom exception carrying state
        boolean coded = false;
        try { throw new AppException("x", 42); } catch (AppException ae) { coded = ae.code == 42; }
        check(coded, "custom-exception-field");
        // nested try / finally execution order
        StringBuilder sb = new StringBuilder();
        try {
            try { sb.append("1"); throw new RuntimeException(); }
            finally { sb.append("2"); }
        } catch (RuntimeException e) { sb.append("3"); }
        check(sb.toString().equals("123"), "nested-try-finally-order");
    }

    // ------------------------------------------------------------------
    static void varargsAndAutobox() {
        check(vlen() == 0 && vlen("a") == 1 && vlen("a", "b") == 2, "varargs-counts");
        check(vlen(new String[]{"a", "b", "c"}) == 3, "varargs-array-passthrough");
        List<Integer> li = listOf(1, 2, 3);
        check(li.size() == 3 && li.get(2) == 3, "safevarargs-generic");
        // Integer cache: -128..127 are interned
        Integer ca = 127, cb = 127;
        check(ca == cb, "autobox-cache-127");
        Integer da = 128, db = 128;
        check(da != db && da.equals(db), "autobox-nocache-128");
        // collection boxing / unboxing
        List<Integer> boxed = new ArrayList<>();
        boxed.add(5);
        int unbox = boxed.get(0);
        check(unbox == 5, "autobox-collection");
        // NPE on unboxing null
        Integer nil = null;
        boolean npeU = false;
        try { int x = nil; npeU = (x == -1); } catch (NullPointerException e) { npeU = true; }
        check(npeU, "unbox-null-npe");
        // ternary that forces unboxing of null
        Integer val = null;
        boolean npeT = false;
        try { int z = true ? val : 0; npeT = (z == -1); } catch (NullPointerException e) { npeT = true; }
        check(npeT, "ternary-unbox-npe");
    }

    // ------------------------------------------------------------------
    static void varAndAnnotations() {
        var list = new ArrayList<String>();
        list.add("v");
        var n = 42;
        var d = 3.14;
        var str = "s";
        check(list.get(0).equals("v") && n == 42 && d == 3.14 && str.equals("s"), "var-local-inference");
        var total = 0;
        for (var i = 0; i < 5; i++) total += i;
        check(total == 10, "var-in-for");
        // runtime annotation reflection
        Tag tag = Tagged.class.getAnnotation(Tag.class);
        check(tag != null && tag.value().equals("demo") && tag.n() == 3
                && tag.tags().length == 2 && tag.tags()[0].equals("x"), "annotation-runtime-elements");
        check(Marked.class.isAnnotationPresent(Marker.class), "annotation-marker-present");
        check(!Tagged.class.isAnnotationPresent(Marker.class), "annotation-absent");
        // annotation default value
        check(Tagged.class.getAnnotation(Tag.class).n() == 3, "annotation-explicit-over-default");
    }
}
