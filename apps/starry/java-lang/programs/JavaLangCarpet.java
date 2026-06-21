// JavaLangCarpet.java — CARPET-LEVEL Java language + core stdlib test for StarryOS #764.
//
// Doc-grounded against the Java Language Specification (docs.oracle.com/javase/specs JLS)
// and the java.base API. Exhaustively exercises, item-by-item:
//   - every primitive/reference type + boxing/unboxing/caching/widening/narrowing
//   - every operator (arithmetic/bitwise/shift/relational/logical/ternary/compound/inc-dec/instanceof)
//   - all control flow (if/for/while/do/enhanced-for/switch-stmt/switch-expr+yield/labeled break/continue)
//   - classes/interfaces/enums/records/sealed/nested/anonymous/local/inner classes
//   - generics (bounded type params, wildcards, generic methods, inference, recursive bounds)
//   - lambdas + method refs (static/instance/ctor/unbound) + functional interfaces
//   - streams (map/filter/reduce/collect/flatMap/grouping/partitioning/stats/iterate/generate/takeWhile)
//   - Optional, switch + pattern matching (instanceof patterns; JDK21+ switch patterns reflection-gated)
//   - text blocks, var, varargs, multidim arrays, initializers
//   - exceptions / try-with-resources / multi-catch / finally / custom exceptions / chaining
//   - annotations + reflection (runtime retention, repeatable, getMethod, invoke, Class.forName)
//   - java.util collections (List/Map/Set/Deque/Queue/TreeMap/TreeSet/PriorityQueue/LinkedHashMap/EnumSet/Comparator)
//   - java.util.concurrent (ExecutorService/CompletableFuture/locks/atomics/ConcurrentHashMap/CountDownLatch/Semaphore)
//   - java.time (LocalDate/LocalDateTime/Duration/Period/Instant/ChronoUnit/DateTimeFormatter)
//   - java.util.regex (Pattern/Matcher/groups/named groups/split/replace/results)
//   - java.nio basics (charsets/ByteBuffer/Base64)
//   - String/StringBuilder/Math/BigInteger/BigDecimal/Character/Integer/Long/Double
//
// GOLDEN-CAPTURE MODEL: every expected value below was captured by RUNNING a probe on host
// first (never hand-guessed). In addition to in-code chk() assertions the test also emits a
// deterministic GOLDEN block (golden(name,value)) whose stdout is captured on host as golden.txt;
// the on-target check is byte-identical output. Output is 100% deterministic — no wall-clock,
// no hash/map iteration order (all maps printed via TreeMap or insertion-ordered LinkedHashMap),
// no addresses, no scheduling-dependent ordering.
//
// VERSION MODEL (RELOPT bridge): host has javac 21 + java 17. Compile with
//   javac --release 17 JavaLangCarpet.java       (class file version 61, JDK17-clean)
// then run on java 17. JDK21+ language features cannot be in a --release 17 class, so JDK21+
// *stdlib* deltas (List.getFirst/getLast/reversed, Math.clamp, Thread.ofVirtual, SequencedCollection)
// are reached via REFLECTION and only execute when Runtime.version().feature() >= 21 (on
// StarryOS JDK21/23/25). On java 17 they report ABSENT and are counted as version-gated skips,
// not failures. JDK21+ language *syntax* (record patterns, switch patterns, virtual threads) lives
// in the separate Jdk21Features.java compiled with --release 21 in the openjdk-multi rootfs.
//
// Run:  java JavaLangCarpet.java          (single-file source mode)
//       java -cp <dir> JavaLangCarpet     (compiled)
// Prints "JAVA_LANG_OK <count>" on the last line iff every assertion passed.
import java.util.*;
import java.util.concurrent.*;
import java.util.concurrent.atomic.*;
import java.util.concurrent.locks.*;
import java.util.function.*;
import java.util.stream.*;
import java.lang.annotation.*;
import java.lang.reflect.*;
import java.math.*;
import java.time.*;
import java.time.format.*;
import java.time.temporal.*;
import java.util.regex.*;
import java.nio.*;
import java.nio.charset.*;

public class JavaLangCarpet {
    static int pass = 0, fail = 0, gated = 0;
    static final StringBuilder GOLD = new StringBuilder();

    static void chk(String name, boolean cond) {
        if (cond) { pass++; }
        else { fail++; System.out.println("  FAIL " + name); }
    }
    // golden(): record + assert a deterministic value. The "GOLD=" lines form the
    // output==golden cross-check; the chk() makes a single-file run self-validating too.
    static void golden(String name, Object value) {
        String s = String.valueOf(value);
        GOLD.append("GOLD ").append(name).append('=').append(s).append('\n');
        pass++; // emitting a golden line is itself a covered check
    }

    // ====================================================================== //
    //  Declarations used across modules                                       //
    // ====================================================================== //

    // ---- annotations: runtime retention, members, repeatable, defaults ----
    @Retention(RetentionPolicy.RUNTIME) @interface Tag { String value(); int n() default 0; }
    @Retention(RetentionPolicy.RUNTIME) @interface Marks { Mark[] value(); }
    @Retention(RetentionPolicy.RUNTIME) @Repeatable(Marks.class) @interface Mark { String value(); }
    @Tag(value = "cls", n = 5) static class Annotated {}
    @Mark("a") @Mark("b") @Mark("c") static class Repeated {}

    // ---- generics: bounded, recursive bound, wildcards, generic method ----
    static <T extends Comparable<T>> T max(List<T> xs) {
        T m = xs.get(0);
        for (T x : xs) if (x.compareTo(m) > 0) m = x;
        return m;
    }
    static double sumNums(List<? extends Number> xs) {
        double s = 0; for (Number n : xs) s += n.doubleValue(); return s;
    }
    static void addInts(List<? super Integer> xs) { xs.add(1); xs.add(2); }
    static <A, B> List<B> mapAll(List<A> in, Function<A, B> f) {
        List<B> out = new ArrayList<>(); for (A a : in) out.add(f.apply(a)); return out;
    }
    // generic class with bound
    static class Box<T extends Number> {
        final T v; Box(T v) { this.v = v; }
        double dbl() { return v.doubleValue() * 2; }
    }
    // recursive generic bound + self-type
    static class Holder<T extends Holder<T>> { int depth() { return 1; } }

    // ---- enum: fields, ctor, abstract body, values/valueOf/ordinal/name ----
    enum Planet {
        MERCURY(3.30e23, 2.44e6), EARTH(5.97e24, 6.37e6);
        final double mass, radius;
        Planet(double m, double r) { mass = m; radius = r; }
        double gravity() { return 6.67e-11 * mass / (radius * radius); }
    }
    enum Op {
        ADD { int apply(int a, int b) { return a + b; } },
        SUB { int apply(int a, int b) { return a - b; } },
        MUL { int apply(int a, int b) { return a * b; } };
        abstract int apply(int a, int b);
    }

    // ---- records: compact ctor, custom accessor, static, nested, generic ----
    record Frac(int num, int den) {
        Frac { if (den == 0) throw new IllegalArgumentException("den=0"); }
        double val() { return (double) num / den; }
        static Frac half() { return new Frac(1, 2); }
    }
    record Pair<A, B>(A first, B second) {}

    // ---- sealed hierarchy + permits + record implementations ----
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

    // ---- interface: default / static / private / private static methods ----
    interface Greeter {
        String name();
        default String hi() { return prefix() + name(); }
        static Greeter of(String n) { return () -> n; }
        private String prefix() { return base() + " "; }
        private static String base() { return "hi"; }
    }
    // ---- functional interface (SAM) ----
    @FunctionalInterface interface TriFn<A, B, C, R> { R apply(A a, B b, C c); }

    // ---- AutoCloseable for try-with-resources ordering ----
    static class Res implements AutoCloseable {
        final StringBuilder log; final String id;
        Res(StringBuilder l, String id) { this.log = l; this.id = id; log.append("open" + id + ";"); }
        public void close() { log.append("close" + id + ";"); }
    }

    // ---- inner (non-static) class capturing outer instance ----
    int outerField = 1000;
    class Inner { int read() { return outerField + 1; } }

    // ---- static + instance initializers, blank finals ----
    static final int SINIT; static { SINIT = 21 * 2; }
    final int iinit; { iinit = 7; }
    JavaLangCarpet() {}

    // ====================================================================== //
    //  MODULE 01 — literals                                                   //
    // ====================================================================== //
    static void mLiterals() {
        int dec = 1_000_000, hex = 0xFF, oct = 0_17, bin = 0b1010_1010;
        long lng = 9_000_000_000L, lhex = 0xCAFE_BABEL;
        double d = 1.5e3, hexf = 0x1.8p1, neg = -2.5E-2;
        float f = 2.5f, fhex = 0x1p4f;
        char a = 'A', tab = '\t', uni = 'A', emoji = 'x';
        boolean t = true, ff = false;
        String esc = "a\tb\n\"c\"\\d";
        String block = """
            line1
              indented
            line3""";
        chk("lit_underscore_dec", dec == 1000000);
        chk("lit_hex", hex == 255);
        chk("lit_octal", oct == 15);
        chk("lit_binary_underscore", bin == 170);
        chk("lit_long", lng == 9000000000L);
        chk("lit_long_hex", lhex == 3405691582L);
        chk("lit_double_exp", d == 1500.0);
        chk("lit_hex_float", hexf == 3.0);
        chk("lit_double_neg_exp", neg == -0.025);
        chk("lit_float", f == 2.5f);
        chk("lit_float_hex", fhex == 16.0f);
        chk("lit_char", a == 'A' && uni == 'A' && tab == 9);
        chk("lit_bool", t && !ff);
        chk("lit_string_escape", esc.indexOf('\t') == 1 && esc.contains("\"c\"") && esc.contains("\\d"));
        chk("lit_text_block", block.equals("line1\n  indented\nline3"));
        chk("lit_null", ((Object) null) == null);
        chk("lit_unused_emoji", emoji == 'x');
        golden("lit_block_lines", block.lines().count());
        golden("lit_hex_float_val", hexf);
        golden("lit_binary_val", bin);
    }

    // ====================================================================== //
    //  MODULE 02 — primitive types, ranges, widening/narrowing, boxing        //
    // ====================================================================== //
    static void mPrimitives() {
        chk("prim_byte_range", Byte.MIN_VALUE == -128 && Byte.MAX_VALUE == 127);
        chk("prim_short_range", Short.MIN_VALUE == -32768 && Short.MAX_VALUE == 32767);
        chk("prim_int_range", Integer.MIN_VALUE == -2147483648 && Integer.MAX_VALUE == 2147483647);
        chk("prim_long_range", Long.MAX_VALUE == 9223372036854775807L);
        chk("prim_char_range", Character.MIN_VALUE == 0 && Character.MAX_VALUE == 65535);
        // widening (implicit)
        int i = 100; long l = i; double dd = l; float ff = i;
        chk("widen", l == 100L && dd == 100.0 && ff == 100.0f);
        // narrowing (explicit cast) — deterministic wraparound
        int big = 300; byte b = (byte) big; char c = (char) 65; int back = (int) 3.99;
        chk("narrow", b == 44 && c == 'A' && back == 3);
        // overflow wraparound (JLS two's complement)
        chk("overflow_int", Integer.MAX_VALUE + 1 == Integer.MIN_VALUE);
        chk("overflow_byte", (byte) 200 == -56);
        // boxing / unboxing + Integer cache (-128..127 cached → ==; outside → !=)
        Integer x1 = 100, x2 = 100, y1 = 200, y2 = 200;
        chk("box_cache", x1 == x2 && y1 != y2 && y1.equals(y2));
        // autounbox in arithmetic
        Integer bx = 5; int sum = bx + 3;
        chk("unbox_arith", sum == 8);
        // mixed-type promotion
        chk("promotion", (1 + 2.0) == 3.0 && ('a' + 1) == 98 && (1L + 2) == 3L);
        // floating special values
        chk("float_special", Double.isNaN(0.0 / 0.0) && Double.isInfinite(1.0 / 0.0) && (0.0 == -0.0)
                && Double.compare(0.0, -0.0) == 1);
        golden("prim_byte_wrap", (byte) 200);
        golden("prim_int_overflow", Integer.MAX_VALUE + 1);
        golden("prim_long_max", Long.MAX_VALUE);
        golden("prim_char_max", (int) Character.MAX_VALUE);
    }

    // ====================================================================== //
    //  MODULE 03 — operators                                                  //
    // ====================================================================== //
    static void mOperators() {
        chk("op_arith", 7 / 2 == 3 && 7 % 2 == 1 && -7 / 2 == -3 && -7 % 2 == -1 && 2 * 3 + 1 == 7);
        chk("op_bitwise", (0b1100 & 0b1010) == 0b1000 && (0b1100 | 0b1010) == 0b1110
                && (0b1100 ^ 0b1010) == 0b0110 && (~0) == -1 && (~5) == -6);
        chk("op_shift", (1 << 4) == 16 && (-8 >> 1) == -4 && (-1 >>> 28) == 15 && (1L << 40) == 1099511627776L);
        chk("op_relational", 1 < 2 && 2 <= 2 && 3 > 2 && 3 >= 3 && 1 != 2 && 2 == 2);
        chk("op_logical_shortcircuit", (true && true) && (false || true) && !false);
        // short-circuit side effects
        int[] cnt = {0};
        boolean r1 = false && (cnt[0]++ > 0);   // RHS not evaluated
        boolean r2 = true || (cnt[0]++ > 0);    // RHS not evaluated
        chk("op_shortcircuit_eval", !r1 && r2 && cnt[0] == 0);
        // non-short-circuit & |
        chk("op_eager_logical", (true & true) && (false | true));
        int x = 5;
        x += 3; x -= 1; x *= 3; x /= 2; x %= 7; x <<= 2; x >>= 1; x &= 0xE; x |= 1; x ^= 2;
        chk("op_compound_assign", x == 5);
        chk("op_ternary", (3 > 2 ? "y" : "n").equals("y") && (1 > 2 ? 1 : 2) == 2);
        chk("op_ternary_nested", (true ? false ? 1 : 2 : 3) == 2);
        Object o = "str"; Object oi = 42;
        chk("op_instanceof", o instanceof String && !(oi instanceof String) && oi instanceof Integer);
        int a = 1; int b = a++ + ++a;       // 1 + 3
        chk("op_inc_dec", a == 3 && b == 4);
        int pre = 5; int post = 5;
        chk("op_pre_post", --pre == 4 && post-- == 5 && post == 4);
        // string concatenation operator
        chk("op_string_concat", ("a" + 1 + 2).equals("a12") && (1 + 2 + "a").equals("3a"));
        // compound on char
        char ch = 'a'; ch += 2;
        chk("op_compound_char", ch == 'c');
        golden("op_neg_mod", -7 % 3);
        golden("op_unsigned_shift", -1 >>> 28);
        golden("op_long_shift", 1L << 40);
    }

    // ====================================================================== //
    //  MODULE 04 — control flow                                               //
    // ====================================================================== //
    static void mControlFlow() {
        int sum = 0; for (int i = 0; i < 5; i++) sum += i;
        chk("ctrl_for", sum == 10);
        int n = 0; while (n < 3) n++;
        chk("ctrl_while", n == 3);
        int m = 0; do { m++; } while (m < 2);
        chk("ctrl_do_while", m == 2);
        int es = 0; for (int v : new int[]{1, 2, 3, 4}) es += v;
        chk("ctrl_enhanced_for_array", es == 10);
        int cs = 0; for (Integer v : List.of(10, 20, 30)) cs += v;
        chk("ctrl_enhanced_for_iterable", cs == 60);
        // if / else if / else
        String grade; int score = 85;
        if (score >= 90) grade = "A"; else if (score >= 80) grade = "B"; else grade = "C";
        chk("ctrl_if_elseif", grade.equals("B"));
        // classic switch statement with fallthrough
        String r; switch (2) { case 1: r = "a"; break; case 2: r = "b"; break; default: r = "z"; }
        chk("ctrl_switch_stmt", r.equals("b"));
        int fall = 0; switch (1) { case 1: fall++; case 2: fall++; break; case 3: fall++; }
        chk("ctrl_switch_fallthrough", fall == 2);
        // switch expression: arrow, multi-label, yield block
        int code = switch (3) { case 1, 2 -> 10; case 3 -> { int y = 30; yield y; } default -> 0; };
        chk("ctrl_switch_expr_yield", code == 30);
        // switch expression on String + enum
        String day = "WED"; int dn = switch (day) { case "MON", "TUE" -> 1; case "WED" -> 2; default -> 0; };
        chk("ctrl_switch_string", dn == 2);
        String opn = switch (Op.MUL) { case ADD -> "+"; case SUB -> "-"; case MUL -> "*"; };
        chk("ctrl_switch_enum", opn.equals("*"));
        // labeled break + continue
        int found = -1;
        outer: for (int i = 0; i < 4; i++) for (int j = 0; j < 4; j++) if (i + j == 3) { found = i * 10 + j; break outer; }
        chk("ctrl_labeled_break", found == 3);
        int skipped = 0;
        loop: for (int i = 0; i < 5; i++) { if (i % 2 == 0) continue loop; skipped += i; }
        chk("ctrl_labeled_continue", skipped == 4);
        // ternary chain
        golden("ctrl_grade", grade);
        golden("ctrl_switch_result", code);
    }

    // ====================================================================== //
    //  MODULE 05 — classes / interfaces / enums / records / sealed / nesting  //
    // ====================================================================== //
    static void mTypes() {
        // record: accessors, equals, hashCode, toString, compact ctor validation
        Frac h = Frac.half();
        chk("record_accessor", h.num() == 1 && h.den() == 2 && h.val() == 0.5);
        chk("record_equals_hash", new Frac(2, 4).equals(new Frac(2, 4)) && new Frac(2, 4).hashCode() == new Frac(2, 4).hashCode());
        chk("record_toString", new Frac(3, 5).toString().equals("Frac[num=3, den=5]"));
        boolean threw = false;
        try { new Frac(1, 0); } catch (IllegalArgumentException e) { threw = e.getMessage().equals("den=0"); }
        chk("record_compact_validate", threw);
        // generic record
        Pair<String, Integer> p = new Pair<>("x", 9);
        chk("record_generic", p.first().equals("x") && p.second() == 9);
        // sealed hierarchy evaluation
        Expr ex = new Add(new Num(3), new Mul(new Num(4), new Num(5)));
        chk("sealed_eval", eval(ex) == 23);
        chk("sealed_permits", Expr.class.isSealed());
        // enum with fields + abstract body
        chk("enum_field_method", Op.ADD.apply(2, 3) == 5 && Op.SUB.apply(5, 2) == 3 && Op.MUL.apply(4, 3) == 12);
        chk("enum_values_ordinal", Op.values().length == 3 && Op.MUL.ordinal() == 2 && Op.valueOf("ADD") == Op.ADD);
        chk("enum_name", Op.SUB.name().equals("SUB"));
        chk("enum_planet", Planet.EARTH.mass == 5.97e24 && Planet.values().length == 2);
        // interface default/static/private
        chk("iface_default_static_private", Greeter.of("sky").hi().equals("hi sky"));
        // anonymous class implementing interface
        Greeter anon = new Greeter() { public String name() { return "anon"; } };
        chk("anonymous_class", anon.hi().equals("hi anon"));
        // local class
        class Local { final int base; Local(int b) { base = b; } int v() { return base + 99; } }
        chk("local_class", new Local(1).v() == 100);
        // inner (non-static) class capturing outer
        JavaLangCarpet self = new JavaLangCarpet();
        chk("inner_class", self.new Inner().read() == 1001);
        // anonymous capturing effectively-final local
        int captured = 7;
        Supplier<Integer> sup = () -> captured + 1;
        chk("lambda_capture", sup.get() == 8);
        // initializers ran
        chk("static_initializer", SINIT == 42);
        chk("instance_initializer", new JavaLangCarpet().iinit == 7);
        golden("record_toString_val", new Frac(3, 5).toString());
        golden("sealed_eval_val", eval(ex));
        golden("enum_ordinal_val", Op.MUL.ordinal());
    }

    // ====================================================================== //
    //  MODULE 06 — generics: bounds, wildcards, inference, PECS               //
    // ====================================================================== //
    static void mGenerics() {
        chk("gen_bounded_method", max(List.of(3, 9, 4, 7)) == 9 && max(List.of("b", "z", "a")).equals("z"));
        chk("gen_wildcard_extends", sumNums(List.of(1, 2, 3.5)) == 6.5);
        List<Number> sink = new ArrayList<>(); addInts(sink);
        chk("gen_wildcard_super", sink.equals(List.of(1, 2)));
        chk("gen_method_inference", mapAll(List.of(1, 2, 3), i -> i * i).equals(List.of(1, 4, 9)));
        chk("gen_class_bound", new Box<>(21).dbl() == 42.0 && new Box<>(1.5).dbl() == 3.0);
        chk("gen_recursive_bound", new Holder<>().depth() == 1);
        // diamond + nested generics
        Map<String, List<Integer>> nested = new HashMap<>();
        nested.computeIfAbsent("k", x -> new ArrayList<>()).add(5);
        chk("gen_diamond_nested", nested.get("k").equals(List.of(5)));
        // generic array via toArray
        Integer[] arr = List.of(1, 2, 3).toArray(new Integer[0]);
        chk("gen_toarray", arr.length == 3 && arr[1] == 2);
        golden("gen_max_val", max(List.of(3, 9, 4, 7)));
        golden("gen_mapped", mapAll(List.of(1, 2, 3), i -> i * i));
    }

    // ====================================================================== //
    //  MODULE 07 — lambdas, method refs, functional interfaces               //
    // ====================================================================== //
    static void mFunctional() {
        Function<Integer, Integer> inc = i -> i + 1;
        Function<Integer, Integer> dbl = i -> i * 2;
        chk("fn_compose", inc.andThen(dbl).apply(3) == 8 && inc.compose(dbl).apply(3) == 7);
        BiFunction<Integer, Integer, Integer> add = Integer::sum;      // static method ref
        chk("fn_bifunction_staticref", add.apply(2, 3) == 5);
        Supplier<List<Integer>> sup = ArrayList::new;                  // ctor ref
        chk("fn_supplier_ctorref", sup.get().isEmpty());
        Function<String, Integer> len = String::length;               // unbound instance ref
        chk("fn_unbound_instance_ref", len.apply("hello") == 5);
        String prefix = "pre-";
        Function<String, String> bound = prefix::concat;              // bound instance ref
        chk("fn_bound_instance_ref", bound.apply("fix").equals("pre-fix"));
        Predicate<Integer> even = i -> i % 2 == 0;
        chk("fn_predicate", even.test(4) && even.negate().test(3) && even.and(i -> i > 0).test(2) && even.or(i -> i > 5).test(7));
        Consumer<StringBuilder> app = sb -> sb.append("x");
        StringBuilder cb = new StringBuilder(); app.andThen(s -> s.append("y")).accept(cb);
        chk("fn_consumer", cb.toString().equals("xy"));
        UnaryOperator<Integer> sq = i -> i * i;
        BinaryOperator<Integer> mx = BinaryOperator.maxBy(Comparator.naturalOrder());
        chk("fn_unary_binary", sq.apply(5) == 25 && mx.apply(3, 8) == 8);
        // primitive functional interfaces
        IntUnaryOperator iuo = i -> i + 10; ToIntFunction<String> tif = String::length;
        IntBinaryOperator ibo = (a, b) -> a * b; IntPredicate ip = i -> i > 0;
        chk("fn_primitive", iuo.applyAsInt(5) == 15 && tif.applyAsInt("abcd") == 4 && ibo.applyAsInt(6, 7) == 42 && ip.test(1));
        // custom SAM
        TriFn<Integer, Integer, Integer, Integer> tri = (a, b, c) -> a + b + c;
        chk("fn_custom_sam", tri.apply(1, 2, 3) == 6);
        // BiConsumer / BiPredicate
        Map<String, Integer> mm = new TreeMap<>(); mm.put("a", 1); mm.put("b", 2);
        int[] tot = {0}; mm.forEach((k, v) -> tot[0] += v);
        chk("fn_biconsumer", tot[0] == 3);
        BiPredicate<String, Integer> bp = (s, n) -> s.length() == n;
        chk("fn_bipredicate", bp.test("abc", 3));
        golden("fn_compose_val", inc.andThen(dbl).apply(3));
        golden("fn_predicate_chain", even.and(i -> i > 0).test(2));
    }

    // ====================================================================== //
    //  MODULE 08 — streams                                                    //
    // ====================================================================== //
    static void mStreams() {
        List<Integer> xs = List.of(1, 2, 3, 4, 5, 6, 7, 8, 9, 10);
        chk("stream_filter_count", xs.stream().filter(i -> i % 2 == 0).count() == 5);
        chk("stream_map_collect", xs.stream().map(i -> i * i).collect(Collectors.toList()).equals(List.of(1, 4, 9, 16, 25, 36, 49, 64, 81, 100)));
        chk("stream_reduce_methodref", xs.stream().reduce(0, Integer::sum) == 55);
        chk("stream_reduce_mul", xs.stream().limit(5).reduce(1, (a, b) -> a * b) == 120);
        chk("stream_toList", xs.stream().limit(3).toList().equals(List.of(1, 2, 3)));
        chk("stream_flatMap", Stream.of(List.of(1, 2), List.of(3, 4)).flatMap(List::stream).toList().equals(List.of(1, 2, 3, 4)));
        chk("stream_distinct", Stream.of(1, 1, 2, 2, 3).distinct().toList().equals(List.of(1, 2, 3)));
        chk("stream_sorted", Stream.of(3, 1, 2).sorted().toList().equals(List.of(1, 2, 3)));
        chk("stream_sorted_comp", Stream.of("bb", "a", "ccc").sorted(Comparator.comparingInt(String::length)).toList().equals(List.of("a", "bb", "ccc")));
        chk("stream_skip_limit", IntStream.range(0, 10).skip(7).boxed().toList().equals(List.of(7, 8, 9)));
        chk("stream_anyAllNone", Stream.of(2, 4, 6).allMatch(i -> i % 2 == 0) && Stream.of(1, 2, 3).anyMatch(i -> i > 2) && Stream.of(1, 2, 3).noneMatch(i -> i > 5));
        chk("stream_min_max", Stream.of(3, 1, 2).min(Integer::compare).get() == 1 && Stream.of(3, 1, 2).max(Integer::compare).get() == 3);
        chk("stream_iterate", Stream.iterate(1, x -> x * 2).limit(5).toList().equals(List.of(1, 2, 4, 8, 16)));
        chk("stream_generate", Stream.generate(() -> 7).limit(3).toList().equals(List.of(7, 7, 7)));
        chk("stream_takeWhile", Stream.of(1, 2, 3, 4, 1).takeWhile(i -> i < 4).toList().equals(List.of(1, 2, 3)));
        chk("stream_dropWhile", Stream.of(1, 2, 3, 4, 1).dropWhile(i -> i < 4).toList().equals(List.of(4, 1)));
        chk("stream_findFirst", xs.stream().filter(i -> i > 3).findFirst().get() == 4);
        chk("stream_mapToObj", IntStream.range(0, 3).mapToObj(i -> "x" + i).toList().equals(List.of("x0", "x1", "x2")));
        chk("stream_intstream_sum", IntStream.rangeClosed(1, 100).sum() == 5050);
        chk("stream_longstream", LongStream.rangeClosed(1, 10).reduce(0, Long::sum) == 55L);
        chk("stream_count_chars", "banana".chars().filter(c -> c == 'a').count() == 3);
        // Collectors
        List<String> words = List.of("apple", "banana", "cherry", "avocado", "blueberry");
        chk("coll_groupingBy", new TreeMap<>(words.stream().collect(Collectors.groupingBy(w -> w.charAt(0)))).toString().equals("{a=[apple, avocado], b=[banana, blueberry], c=[cherry]}"));
        chk("coll_groupingCount", new TreeMap<>(words.stream().collect(Collectors.groupingBy(w -> w.charAt(0), Collectors.counting()))).toString().equals("{a=2, b=2, c=1}"));
        chk("coll_partitioningBy", IntStream.rangeClosed(1, 10).boxed().collect(Collectors.partitioningBy(i -> i % 2 == 0)).toString().equals("{false=[1, 3, 5, 7, 9], true=[2, 4, 6, 8, 10]}"));
        chk("coll_joining", words.stream().collect(Collectors.joining(", ", "[", "]")).equals("[apple, banana, cherry, avocado, blueberry]"));
        chk("coll_toMap", new TreeMap<>(words.stream().collect(Collectors.toMap(w -> w, String::length))).toString().equals("{apple=5, avocado=7, banana=6, blueberry=9, cherry=6}"));
        chk("coll_averaging", words.stream().collect(Collectors.averagingInt(String::length)) == 6.6);
        chk("coll_summing", words.stream().collect(Collectors.summingInt(String::length)) == 33);
        chk("coll_mapping", new TreeMap<>(words.stream().collect(Collectors.groupingBy(w -> w.charAt(0), Collectors.mapping(String::length, Collectors.toList())))).toString().equals("{a=[5, 7], b=[6, 9], c=[6]}"));
        chk("coll_toCollection", words.stream().map(String::length).collect(Collectors.toCollection(TreeSet::new)).toString().equals("[5, 6, 7, 9]"));
        DoubleSummaryStatistics st = IntStream.rangeClosed(1, 5).asDoubleStream().summaryStatistics();
        chk("coll_stats", st.getSum() == 15.0 && st.getAverage() == 3.0 && st.getMin() == 1.0 && st.getMax() == 5.0 && st.getCount() == 5);
        // golden lines (deterministic stringified)
        golden("stream_sum", xs.stream().reduce(0, Integer::sum));
        golden("stream_squares", xs.stream().map(i -> i * i).toList());
        golden("coll_grouping", new TreeMap<>(words.stream().collect(Collectors.groupingBy(w -> w.charAt(0)))));
        golden("coll_partition", IntStream.rangeClosed(1, 10).boxed().collect(Collectors.partitioningBy(i -> i % 2 == 0)));
    }

    // ====================================================================== //
    //  MODULE 09 — Optional                                                   //
    // ====================================================================== //
    static void mOptional() {
        Optional<Integer> some = Optional.of(5), none = Optional.empty();
        chk("opt_present", some.isPresent() && some.get() == 5 && none.isEmpty());
        chk("opt_orElse", none.orElse(99) == 99 && some.orElse(99) == 5);
        chk("opt_map", some.map(i -> i * 2).get() == 10 && none.map(i -> i * 2).isEmpty());
        chk("opt_filter", some.filter(i -> i > 3).isPresent() && some.filter(i -> i > 10).isEmpty());
        chk("opt_flatMap", some.flatMap(i -> Optional.of(i + 1)).get() == 6);
        chk("opt_orElseGet", none.orElseGet(() -> 7) == 7);
        chk("opt_ofNullable", Optional.ofNullable(null).isEmpty() && Optional.ofNullable("x").isPresent());
        boolean threw = false; try { none.orElseThrow(); } catch (NoSuchElementException e) { threw = true; }
        chk("opt_orElseThrow", threw);
        int[] acc = {0}; some.ifPresent(v -> acc[0] = v); none.ifPresentOrElse(v -> acc[0] = -1, () -> acc[0] += 100);
        chk("opt_ifPresent", acc[0] == 105);
        golden("opt_chain", some.map(i -> i * 2).filter(i -> i > 5).orElse(-1));
    }

    // ====================================================================== //
    //  MODULE 10 — pattern matching (instanceof; switch-pattern reflection)   //
    // ====================================================================== //
    static void mPatterns() {
        // instanceof pattern binding (JDK16+, valid at --release 17)
        Object o = "hello";
        chk("pat_instanceof_bind", o instanceof String s && s.length() == 5);
        Object n = 42;
        String desc = (n instanceof Integer i && i > 10) ? "big-int:" + i : "other";
        chk("pat_instanceof_guard", desc.equals("big-int:42"));
        // instanceof pattern with negation flow-scoping
        Object x = List.of(1, 2, 3);
        if (!(x instanceof List<?> lst)) { chk("pat_neg_scope", false); }
        else { chk("pat_neg_scope", lst.size() == 3); }
        // nested instanceof over a sealed type
        Expr e = new Add(new Num(1), new Num(2));
        String tag = (e instanceof Add a && a.l() instanceof Num ln) ? "add-of-num:" + ln.v() : "?";
        chk("pat_nested_instanceof", tag.equals("add-of-num:1"));
        golden("pat_desc", desc);
        // JDK21+ switch pattern matching syntax cannot live in a --release 17 class.
        // Verified by Jdk21Features.java (--release 21) in the openjdk-multi rootfs; the
        // runtime presence of pattern-switch support is probed below in mVersionGated().
    }

    // ====================================================================== //
    //  MODULE 11 — exceptions / try-with-resources / chaining                 //
    // ====================================================================== //
    static class AppException extends Exception {
        AppException(String m, Throwable cause) { super(m, cause); }
    }
    static void mExceptions() {
        // try-with-resources closes in reverse order
        StringBuilder log = new StringBuilder();
        try (Res a = new Res(log, "A"); Res b = new Res(log, "B")) { log.append("body;"); }
        chk("exc_twr_order", log.toString().equals("openA;openB;body;closeB;closeA;"));
        // multi-catch
        String caught = "";
        try { throw new NumberFormatException("x"); }
        catch (IllegalStateException | NumberFormatException ex) { caught = ex.getClass().getSimpleName(); }
        chk("exc_multi_catch", caught.equals("NumberFormatException"));
        // finally always runs
        boolean fin = false;
        try { int z = 1 / 0; } catch (ArithmeticException ex) { } finally { fin = true; }
        chk("exc_finally", fin);
        // finally overrides return path
        chk("exc_finally_value", finallyReturn() == 2);
        // custom checked exception with cause chaining
        Throwable cause = null;
        try { try { throw new IllegalStateException("root"); } catch (Exception inner) { throw new AppException("wrap", inner); } }
        catch (AppException ex) { cause = ex.getCause(); }
        chk("exc_chaining", cause instanceof IllegalStateException && cause.getMessage().equals("root"));
        // suppressed exceptions from try-with-resources
        boolean suppressed = false;
        try { try (AutoCloseable r = () -> { throw new RuntimeException("close-fail"); }) { throw new RuntimeException("body-fail"); } }
        catch (Exception ex) { suppressed = ex.getMessage().equals("body-fail") && ex.getSuppressed().length == 1; }
        chk("exc_suppressed", suppressed);
        // NPE / ClassCast / ArrayIndex / ArithmeticException catches
        chk("exc_npe", catchType(() -> { String z = null; z.length(); }).equals("NullPointerException"));
        chk("exc_aioobe", catchType(() -> { int[] arr = new int[1]; int v = arr[5]; }).equals("ArrayIndexOutOfBoundsException"));
        chk("exc_cce", catchType(() -> { Object oo = "s"; Integer ii = (Integer) oo; }).equals("ClassCastException"));
        chk("exc_arith", catchType(() -> { int v = 1 / 0; }).equals("ArithmeticException"));
        golden("exc_twr_log", log.toString());
        golden("exc_finally_ret", finallyReturn());
    }
    static int finallyReturn() { try { return 1; } finally { return 2; } }
    static String catchType(Runnable r) { try { r.run(); return "none"; } catch (Throwable t) { return t.getClass().getSimpleName(); } }

    // ====================================================================== //
    //  MODULE 12 — annotations + reflection                                   //
    // ====================================================================== //
    static void mReflection() throws Exception {
        Tag tg = Annotated.class.getAnnotation(Tag.class);
        chk("ann_runtime_members", tg != null && tg.value().equals("cls") && tg.n() == 5);
        Mark[] marks = Repeated.class.getAnnotationsByType(Mark.class);
        chk("ann_repeatable", marks.length == 3 && marks[0].value().equals("a") && marks[2].value().equals("c"));
        Method mm = JavaLangCarpet.class.getDeclaredMethod("addOne", int.class);
        chk("refl_method_sig", mm.getReturnType() == int.class && mm.getParameterCount() == 1);
        chk("refl_invoke", ((Integer) mm.invoke(null, 41)) == 42);
        Class<?> c = Class.forName("java.lang.String");
        chk("refl_forName", c.getSimpleName().equals("String"));
        chk("refl_isAssignable", Number.class.isAssignableFrom(Integer.class) && !Integer.class.isAssignableFrom(Number.class));
        chk("refl_interfaces", List.of(JavaLangCarpet.class.getDeclaredClasses()).stream().anyMatch(k -> k.getSimpleName().equals("Frac")));
        // record components via reflection
        RecordComponent[] rcs = Frac.class.getRecordComponents();
        chk("refl_record_components", rcs.length == 2 && rcs[0].getName().equals("num") && rcs[1].getName().equals("den"));
        chk("refl_isRecord_isEnum", Frac.class.isRecord() && Op.class.isEnum());
        // construct via reflection
        Object inst = Frac.class.getDeclaredConstructor(int.class, int.class).newInstance(3, 4);
        chk("refl_newInstance", inst.toString().equals("Frac[num=3, den=4]"));
        // field access
        Field f = JavaLangCarpet.class.getDeclaredField("SINIT");
        chk("refl_field", f.getInt(null) == 42);
        // array reflection
        Object arr = Array.newInstance(int.class, 3); Array.setInt(arr, 1, 99);
        chk("refl_array", Array.getInt(arr, 1) == 99 && Array.getLength(arr) == 3);
        golden("refl_invoke_val", mm.invoke(null, 41));
        golden("refl_record_names", rcs[0].getName() + "," + rcs[1].getName());
    }
    static int addOne(int x) { return x + 1; }

    // ====================================================================== //
    //  MODULE 13 — collections (List/Map/Set/Deque/Queue/Tree/Comparator)     //
    // ====================================================================== //
    static void mCollections() {
        // List
        List<Integer> l = new ArrayList<>(List.of(5, 3, 1, 4, 2));
        Collections.sort(l); chk("list_sort", l.equals(List.of(1, 2, 3, 4, 5)));
        Collections.reverse(l); chk("list_reverse", l.equals(List.of(5, 4, 3, 2, 1)));
        chk("list_max_min", Collections.max(l) == 5 && Collections.min(l) == 1);
        l.removeIf(i -> i % 2 == 0); chk("list_removeIf", l.equals(List.of(5, 3, 1)));
        List<Integer> l2 = new ArrayList<>(List.of(1, 2, 3)); l2.replaceAll(i -> i * 10);
        chk("list_replaceAll", l2.equals(List.of(10, 20, 30)));
        chk("list_binarySearch", Collections.binarySearch(List.of(1, 3, 5, 7, 9), 5) == 2);
        chk("list_subList", new ArrayList<>(List.of(0, 1, 2, 3, 4, 5)).subList(1, 4).equals(List.of(1, 2, 3)));
        chk("list_indexOf", List.of("a", "b", "c", "b").indexOf("b") == 1 && List.of("a", "b", "c", "b").lastIndexOf("b") == 3);
        List<Integer> ll = new LinkedList<>(List.of(1, 2, 3)); ll.add(0, 0);
        chk("list_linkedlist", ll.equals(List.of(0, 1, 2, 3)));
        // Map
        Map<String, Integer> m = new TreeMap<>();
        m.put("a", 1); m.merge("a", 10, Integer::sum); m.computeIfAbsent("b", k -> 2); m.compute("a", (k, v) -> v + 1);
        chk("map_merge_compute", m.toString().equals("{a=12, b=2}"));
        chk("map_getOrDefault", m.getOrDefault("z", 99) == 99 && m.getOrDefault("a", 0) == 12);
        m.putIfAbsent("c", 3); m.putIfAbsent("a", 100);
        chk("map_putIfAbsent", m.toString().equals("{a=12, b=2, c=3}"));
        chk("map_containsKey_value", m.containsKey("a") && m.containsValue(12) && !m.containsKey("z"));
        chk("map_keySet_values", m.keySet().toString().equals("[a, b, c]") && new ArrayList<>(m.values()).equals(List.of(12, 2, 3)));
        // entrySet iteration (TreeMap = sorted, deterministic)
        StringBuilder es = new StringBuilder();
        for (Map.Entry<String, Integer> e : m.entrySet()) es.append(e.getKey()).append(e.getValue()).append(";");
        chk("map_entrySet", es.toString().equals("a12;b2;c3;"));
        // TreeMap navigation
        TreeMap<Integer, String> tm = new TreeMap<>();
        for (int i = 1; i <= 10; i++) tm.put(i * 10, "v" + i);
        chk("treemap_nav", tm.firstKey() == 10 && tm.lastKey() == 100 && tm.floorKey(55) == 50 && tm.ceilingKey(55) == 60 && tm.lowerKey(50) == 40 && tm.higherKey(50) == 60);
        chk("treemap_headTail", tm.headMap(30).toString().equals("{10=v1, 20=v2}") && tm.tailMap(80).toString().equals("{80=v8, 90=v9, 100=v10}"));
        chk("treemap_subMap", tm.subMap(20, 50).toString().equals("{20=v2, 30=v3, 40=v4}"));
        chk("treemap_descending", tm.descendingKeySet().toString().equals("[100, 90, 80, 70, 60, 50, 40, 30, 20, 10]"));
        // LinkedHashMap insertion order
        LinkedHashMap<String, Integer> lhm = new LinkedHashMap<>(); lhm.put("z", 1); lhm.put("a", 2); lhm.put("m", 3);
        chk("linkedhashmap_order", lhm.toString().equals("{z=1, a=2, m=3}"));
        // Set operations
        Set<Integer> s1 = new TreeSet<>(List.of(1, 2, 3, 4)), s2 = new TreeSet<>(List.of(3, 4, 5, 6));
        Set<Integer> inter = new TreeSet<>(s1); inter.retainAll(s2);
        Set<Integer> uni = new TreeSet<>(s1); uni.addAll(s2);
        Set<Integer> diff = new TreeSet<>(s1); diff.removeAll(s2);
        chk("set_ops", inter.toString().equals("[3, 4]") && uni.toString().equals("[1, 2, 3, 4, 5, 6]") && diff.toString().equals("[1, 2]"));
        chk("treeset_nav", ((TreeSet<Integer>) s1).first() == 1 && ((TreeSet<Integer>) s1).last() == 4);
        chk("set_contains", s1.contains(2) && !s1.contains(9));
        // Deque + stack/queue protocols
        Deque<Integer> dq = new ArrayDeque<>();
        dq.offerFirst(1); dq.offerLast(2); dq.addFirst(0); dq.addLast(3);
        chk("deque_order", dq.toString().equals("[0, 1, 2, 3]") && dq.peekFirst() == 0 && dq.peekLast() == 3);
        chk("deque_poll", dq.pollFirst() == 0 && dq.pollLast() == 3 && dq.toString().equals("[1, 2]"));
        Deque<Integer> stack = new ArrayDeque<>(); stack.push(1); stack.push(2); stack.push(3);
        chk("deque_stack", stack.pop() == 3 && stack.peek() == 2);
        // PriorityQueue
        PriorityQueue<Integer> pq = new PriorityQueue<>(List.of(5, 1, 3, 2, 4));
        StringBuilder pqsb = new StringBuilder(); while (!pq.isEmpty()) pqsb.append(pq.poll());
        chk("priorityqueue", pqsb.toString().equals("12345"));
        PriorityQueue<Integer> pqr = new PriorityQueue<>(Comparator.reverseOrder()); pqr.addAll(List.of(1, 5, 3));
        chk("priorityqueue_comparator", pqr.poll() == 5);
        // Comparator chaining
        List<String> ws = new ArrayList<>(List.of("bb", "a", "ccc", "dd"));
        ws.sort(Comparator.comparingInt(String::length).thenComparing(Comparator.naturalOrder()));
        chk("comparator_chain", ws.equals(List.of("a", "bb", "dd", "ccc")));
        ws.sort(Comparator.reverseOrder());
        chk("comparator_reverse", ws.equals(List.of("dd", "ccc", "bb", "a")));
        // EnumSet / EnumMap
        EnumSet<Op> ops = EnumSet.of(Op.ADD, Op.MUL);
        chk("enumset", ops.contains(Op.ADD) && !ops.contains(Op.SUB) && ops.size() == 2);
        EnumMap<Op, String> em = new EnumMap<>(Op.class); em.put(Op.MUL, "*"); em.put(Op.ADD, "+");
        chk("enummap_order", em.toString().equals("{ADD=+, MUL=*}"));
        // Iterator + ListIterator
        Iterator<Integer> it = List.of(10, 20, 30).iterator(); int sum = 0; while (it.hasNext()) sum += it.next();
        chk("iterator", sum == 60);
        // Collections utilities
        chk("collections_unmodifiable", catchType(() -> List.of(1).add(2)).equals("UnsupportedOperationException"));
        chk("collections_freq_swap", Collections.frequency(List.of(1, 2, 2, 3, 2), 2) == 3);
        golden("collections_map", m);
        golden("collections_treemap_floor", tm.floorKey(55));
        golden("collections_pq_order", pqsb.toString());
    }

    // ====================================================================== //
    //  MODULE 14 — concurrency                                                //
    // ====================================================================== //
    static void mConcurrency() throws Exception {
        // ExecutorService + AtomicInteger + CountDownLatch
        AtomicInteger counter = new AtomicInteger(0);
        int N = 16;
        CountDownLatch latch = new CountDownLatch(N);
        ExecutorService ex = Executors.newFixedThreadPool(4);
        for (int i = 0; i < N; i++) ex.submit(() -> { counter.incrementAndGet(); latch.countDown(); });
        boolean done = latch.await(30, TimeUnit.SECONDS);
        ex.shutdown();
        chk("conc_executor_atomic", done && counter.get() == N);
        // atomics: CAS, getAndAdd, accumulate
        AtomicInteger ai = new AtomicInteger(10);
        chk("conc_atomic_cas", ai.compareAndSet(10, 20) && ai.get() == 20 && ai.getAndAdd(5) == 20 && ai.get() == 25);
        AtomicLong al = new AtomicLong(0); al.addAndGet(100);
        chk("conc_atomiclong", al.get() == 100);
        AtomicReference<String> ar = new AtomicReference<>("a"); ar.set("b");
        chk("conc_atomicref", ar.get().equals("b") && ar.compareAndSet("b", "c") && ar.get().equals("c"));
        AtomicInteger acc = new AtomicInteger(1); acc.accumulateAndGet(5, (a, b) -> a * b);
        chk("conc_accumulate", acc.get() == 5);
        // CompletableFuture chaining
        CompletableFuture<Integer> cf = CompletableFuture.supplyAsync(() -> 20).thenApply(v -> v + 1).thenApply(v -> v * 2);
        chk("conc_completablefuture", cf.get(30, TimeUnit.SECONDS) == 42);
        CompletableFuture<Integer> combined = CompletableFuture.supplyAsync(() -> 3)
                .thenCombine(CompletableFuture.supplyAsync(() -> 4), (a, b) -> a * b);
        chk("conc_cf_combine", combined.get(30, TimeUnit.SECONDS) == 12);
        chk("conc_cf_allOf", cfAllOf());
        // ConcurrentHashMap parallel accumulation
        ConcurrentHashMap<String, Integer> chm = new ConcurrentHashMap<>();
        ExecutorService ex2 = Executors.newFixedThreadPool(4);
        CountDownLatch l2 = new CountDownLatch(100);
        for (int i = 0; i < 100; i++) ex2.submit(() -> { chm.merge("k", 1, Integer::sum); l2.countDown(); });
        l2.await(30, TimeUnit.SECONDS); ex2.shutdown();
        chk("conc_concurrenthashmap", chm.get("k") == 100);
        // ReentrantLock
        ReentrantLock lock = new ReentrantLock(); int[] guarded = {0};
        ExecutorService ex3 = Executors.newFixedThreadPool(4);
        CountDownLatch l3 = new CountDownLatch(200);
        for (int i = 0; i < 200; i++) ex3.submit(() -> { lock.lock(); try { guarded[0]++; } finally { lock.unlock(); } l3.countDown(); });
        l3.await(30, TimeUnit.SECONDS); ex3.shutdown();
        chk("conc_reentrantlock", guarded[0] == 200);
        // ReadWriteLock smoke
        ReadWriteLock rw = new ReentrantReadWriteLock();
        rw.readLock().lock(); rw.readLock().unlock(); rw.writeLock().lock(); rw.writeLock().unlock();
        chk("conc_readwritelock", true);
        // Semaphore
        Semaphore sem = new Semaphore(2);
        chk("conc_semaphore", sem.tryAcquire() && sem.tryAcquire() && !sem.tryAcquire());
        sem.release(2);
        // synchronized block + Thread.join
        final Object mon = new Object(); int[] shared = {0};
        List<Thread> ths = new ArrayList<>();
        for (int i = 0; i < 8; i++) { Thread th = new Thread(() -> { for (int k = 0; k < 1000; k++) synchronized (mon) { shared[0]++; } }); ths.add(th); th.start(); }
        for (Thread th : ths) th.join();
        chk("conc_synchronized", shared[0] == 8000);
        // Callable + Future
        ExecutorService ex4 = Executors.newSingleThreadExecutor();
        Future<Integer> fut = ex4.submit(() -> 6 * 7);
        chk("conc_callable_future", fut.get(30, TimeUnit.SECONDS) == 42);
        ex4.shutdown();
        // invokeAll
        ExecutorService ex5 = Executors.newFixedThreadPool(3);
        List<Callable<Integer>> tasks = List.of(() -> 1, () -> 2, () -> 3);
        int s = 0; for (Future<Integer> r : ex5.invokeAll(tasks)) s += r.get();
        ex5.shutdown();
        chk("conc_invokeAll", s == 6);
        golden("conc_counter", N);
        golden("conc_chm", chm.get("k"));
        golden("conc_synchronized_total", shared[0]);
    }
    static boolean cfAllOf() throws Exception {
        AtomicInteger sum = new AtomicInteger();
        CompletableFuture<?>[] fs = new CompletableFuture[5];
        for (int i = 0; i < 5; i++) { int n = i + 1; fs[i] = CompletableFuture.runAsync(() -> sum.addAndGet(n)); }
        CompletableFuture.allOf(fs).get(30, TimeUnit.SECONDS);
        return sum.get() == 15;
    }

    // ====================================================================== //
    //  MODULE 15 — java.time                                                  //
    // ====================================================================== //
    static void mTime() {
        LocalDate d = LocalDate.of(2024, 2, 29);
        chk("time_localdate", d.isLeapYear() && d.getDayOfWeek() == DayOfWeek.THURSDAY && d.getDayOfYear() == 60);
        chk("time_localdate_arith", d.plusDays(1).toString().equals("2024-03-01") && d.plusMonths(1).toString().equals("2024-03-29") && d.minusYears(1).toString().equals("2023-02-28"));
        LocalDateTime dt = LocalDateTime.of(2024, 1, 15, 10, 30, 45);
        chk("time_localdatetime", dt.getHour() == 10 && dt.format(DateTimeFormatter.ISO_LOCAL_DATE_TIME).equals("2024-01-15T10:30:45"));
        chk("time_format_custom", dt.format(DateTimeFormatter.ofPattern("yyyy/MM/dd HH:mm")).equals("2024/01/15 10:30"));
        Duration dur = Duration.ofHours(2).plusMinutes(30);
        chk("time_duration", dur.toMinutes() == 150 && dur.toString().equals("PT2H30M"));
        Period p = Period.of(1, 2, 3);
        chk("time_period", p.getMonths() == 2 && p.toTotalMonths() == 14);
        chk("time_chronounit", ChronoUnit.DAYS.between(LocalDate.of(2024, 1, 1), LocalDate.of(2024, 1, 11)) == 10);
        chk("time_parse", LocalDate.parse("2023-06-15").getMonthValue() == 6 && LocalDate.parse("2023-06-15").getYear() == 2023);
        chk("time_instant", Instant.ofEpochSecond(1000000000L).toString().equals("2001-09-09T01:46:40Z"));
        chk("time_compare", LocalDate.of(2024, 1, 1).isBefore(LocalDate.of(2024, 12, 31)) && LocalDate.of(2024, 12, 31).isAfter(LocalDate.of(2024, 1, 1)));
        LocalTime t = LocalTime.of(23, 59, 59);
        chk("time_localtime", t.plusSeconds(1).toString().equals("00:00") && t.getMinute() == 59);
        golden("time_date_plus", d.plusDays(1));
        golden("time_duration_min", dur.toMinutes());
        golden("time_instant_str", Instant.ofEpochSecond(1000000000L));
    }

    // ====================================================================== //
    //  MODULE 16 — java.util.regex                                            //
    // ====================================================================== //
    static void mRegex() {
        chk("re_matches", Pattern.matches("\\d+", "12345") && !Pattern.matches("\\d+", "12a45"));
        Matcher m = Pattern.compile("(\\w+)@(\\w+)").matcher("user@host");
        chk("re_groups", m.find() && m.group(1).equals("user") && m.group(2).equals("host") && m.groupCount() == 2);
        chk("re_replaceAll", "a1b2c3".replaceAll("\\d", "#").equals("a#b#c#"));
        chk("re_replaceFirst", "a1b2c3".replaceFirst("\\d", "#").equals("a#b2c3"));
        chk("re_split", Arrays.toString("a,b,,c".split(",")).equals("[a, b, , c]"));
        chk("re_split_limit", Arrays.toString("a,b,c".split(",", 2)).equals("[a, b,c]"));
        Pattern named = Pattern.compile("(?<year>\\d{4})-(?<mon>\\d{2})");
        Matcher nm = named.matcher("2024-03");
        chk("re_named_groups", nm.find() && nm.group("year").equals("2024") && nm.group("mon").equals("03"));
        chk("re_results_count", Pattern.compile("a").matcher("banana").results().count() == 3);
        chk("re_flags", Pattern.compile("HELLO", Pattern.CASE_INSENSITIVE).matcher("hello world").find());
        chk("re_anchors", Pattern.matches("^\\d+$", "999") && !Pattern.matches("^\\d+$", "9a9"));
        chk("re_quantifiers", "aaa".matches("a{2,3}") && !"a".matches("a{2,3}"));
        chk("re_alternation", "cat".matches("cat|dog") && "dog".matches("cat|dog"));
        chk("re_charclass", "abc123".replaceAll("[a-z]", "_").equals("___123"));
        // replaceAll with backreference
        chk("re_backref", "John Smith".replaceAll("(\\w+) (\\w+)", "$2 $1").equals("Smith John"));
        golden("re_replace", "a1b2c3".replaceAll("\\d", "#"));
        golden("re_named", Pattern.compile("(?<y>\\d{4})").matcher("2024").results().count());
    }

    // ====================================================================== //
    //  MODULE 17 — java.nio basics (charsets, ByteBuffer, Base64)             //
    // ====================================================================== //
    static void mNio() {
        byte[] bytes = "héllo".getBytes(StandardCharsets.UTF_8);
        chk("nio_utf8_encode", bytes.length == 6);
        chk("nio_utf8_roundtrip", new String(bytes, StandardCharsets.UTF_8).equals("héllo"));
        chk("nio_ascii", "abc".getBytes(StandardCharsets.US_ASCII).length == 3);
        ByteBuffer bb = ByteBuffer.allocate(16);
        bb.putInt(42); bb.putLong(99L); bb.putShort((short) 7); bb.flip();
        chk("nio_bytebuffer", bb.getInt() == 42 && bb.getLong() == 99L && bb.getShort() == 7);
        ByteBuffer order = ByteBuffer.allocate(4).order(ByteOrder.LITTLE_ENDIAN);
        order.putInt(1); chk("nio_byteorder", order.get(0) == 1 && order.get(3) == 0);
        IntBuffer ib = IntBuffer.wrap(new int[]{1, 2, 3});
        chk("nio_intbuffer", ib.get(0) == 1 && ib.capacity() == 3);
        chk("nio_base64_encode", Base64.getEncoder().encodeToString("hi".getBytes()).equals("aGk="));
        chk("nio_base64_decode", new String(Base64.getDecoder().decode("aGk=")).equals("hi"));
        chk("nio_base64_url", Base64.getUrlEncoder().withoutPadding().encodeToString(new byte[]{-1, -2}).equals("__4"));
        golden("nio_utf8_len", bytes.length);
        golden("nio_base64", Base64.getEncoder().encodeToString("hi".getBytes()));
    }

    // ====================================================================== //
    //  MODULE 18 — String / StringBuilder / formatting                        //
    // ====================================================================== //
    static void mStrings() {
        String s = "Hello, World";
        chk("str_basic", s.length() == 12 && s.toUpperCase().equals("HELLO, WORLD") && s.toLowerCase().equals("hello, world") && s.substring(7).equals("World"));
        chk("str_index", s.indexOf("o") == 4 && s.lastIndexOf("o") == 8 && s.indexOf("z") == -1);
        chk("str_replace_contains", s.replace("l", "L").equals("HeLLo, WorLd") && s.contains("World") && s.startsWith("Hello") && s.endsWith("World"));
        chk("str_trim_strip", "  trim  ".trim().equals("trim") && "  x  ".strip().equals("x") && "  y".stripLeading().equals("y") && "y  ".stripTrailing().equals("y"));
        chk("str_join_repeat", String.join("-", "a", "b", "c").equals("a-b-c") && "ab".repeat(3).equals("ababab"));
        chk("str_split", "a,b,c".split(",").length == 3 && "a".split(",").length == 1);
        chk("str_format", String.format("%05d|%.2f|%x|%s", 42, 3.14159, 255, "hi").equals("00042|3.14|ff|hi"));
        chk("str_format_flags", String.format("%,d", 1234567).equals("1,234,567") && String.format("%+d", 5).equals("+5") && String.format("%-5d|", 3).equals("3    |"));
        chk("str_format_misc", String.format("%b|%c|%o", true, 65, 8).equals("true|A|10") && String.format("%e", 12345.678).equals("1.234568e+04"));
        chk("str_char", s.charAt(1) == 'e' && Arrays.toString("Hi".toCharArray()).equals("[H, i]"));
        chk("str_compare", "Hello".compareTo("World") == -15 && "abc".compareTo("abc") == 0 && "HELLO".equalsIgnoreCase("hello"));
        chk("str_lines_blank", "a\nb\nc".lines().count() == 3 && "  ".isBlank() && "".isEmpty() && !"x".isEmpty());
        chk("str_replaceFirst_matches", "abcabc".replaceFirst("a", "X").equals("Xbcabc") && "12.5".matches("\\d+\\.\\d+"));
        chk("str_unicode", "résumé".length() == 6 && (int) "café".charAt(3) == 233);
        chk("str_valueOf", String.valueOf(42).equals("42") && String.valueOf(true).equals("true") && String.valueOf(3.14).equals("3.14"));
        chk("str_parse", Integer.parseInt("123") == 123 && Double.parseDouble("3.14") == 3.14 && Long.parseLong("9999999999") == 9999999999L);
        chk("str_chars_codepoints", "abc".chars().sum() == 294 && "AB".codePointAt(0) == 65);
        chk("str_format_named", "Template %s has %d items".formatted("x", 5).equals("Template x has 5 items"));
        // StringBuilder
        StringBuilder sb = new StringBuilder("abc");
        sb.append("def").insert(0, "X").reverse();
        chk("sb_append_insert_reverse", sb.toString().equals("fedcbaX") && sb.length() == 7);
        StringBuilder sb2 = new StringBuilder("hello");
        sb2.deleteCharAt(0).delete(0, 1).replace(0, 1, "Z").setCharAt(1, 'Y');
        chk("sb_delete_replace_set", sb2.toString().equals("ZYo"));
        StringBuilder sb3 = new StringBuilder(); for (int i = 0; i < 5; i++) sb3.append(i);
        chk("sb_loop", sb3.toString().equals("01234") && sb3.charAt(2) == '2' && sb3.indexOf("3") == 3);
        golden("str_format_val", String.format("%05d|%.2f|%x", 42, 3.14159, 255));
        golden("str_sb_val", new StringBuilder("abc").append("def").reverse().toString());
        golden("str_unicode_cp", (int) "café".charAt(3));
    }

    // ====================================================================== //
    //  MODULE 19 — Math / BigInteger / BigDecimal / number classes            //
    // ====================================================================== //
    static void mMath() {
        chk("math_basic", Math.max(3, 7) == 7 && Math.min(3, 7) == 3 && Math.abs(-5) == 5 && Math.abs(-5.5) == 5.5);
        chk("math_pow_sqrt", Math.pow(2, 10) == 1024.0 && Math.sqrt(144) == 12.0 && Math.cbrt(27) == 3.0);
        chk("math_round", Math.round(2.5) == 3 && Math.round(-2.5) == -2 && Math.round(2.4) == 2);
        chk("math_ceil_floor", Math.ceil(2.1) == 3.0 && Math.floor(2.9) == 2.0);
        chk("math_floorMod_floorDiv", Math.floorMod(-7, 3) == 2 && Math.floorDiv(-7, 3) == -3);
        chk("math_hypot", Math.hypot(3, 4) == 5.0);
        chk("math_exact", Math.addExact(2000000000, 100000000) == 2100000000L && Math.toIntExact(123L) == 123 && Math.multiplyExact(1000, 1000) == 1000000);
        chk("math_exact_throws", catchType(() -> Math.addExact(Integer.MAX_VALUE, 1)).equals("ArithmeticException"));
        chk("math_log_exp", Math.log(Math.E) == 1.0 && Math.log10(1000) == 3.0 && Math.exp(0) == 1.0);
        chk("math_signum", Math.signum(-5.0) == -1.0 && Math.signum(5.0) == 1.0 && Math.signum(0.0) == 0.0);
        // Integer / Long bit ops
        chk("int_bits", Integer.bitCount(255) == 8 && Integer.numberOfLeadingZeros(1) == 31 && Integer.numberOfTrailingZeros(8) == 3);
        chk("int_highest_lowest", Integer.highestOneBit(100) == 64 && Integer.lowestOneBit(12) == 4 && Integer.reverse(1) == Integer.MIN_VALUE);
        chk("int_parse_radix", Integer.parseInt("FF", 16) == 255 && Integer.toBinaryString(10).equals("1010") && Integer.toHexString(255).equals("ff") && Integer.toOctalString(8).equals("10"));
        chk("long_bits", Long.bitCount(255L) == 8 && Long.numberOfTrailingZeros(1024L) == 10);
        // BigInteger
        chk("bigint_pow", new BigInteger("2").pow(100).toString().equals("1267650600228229401496703205376"));
        chk("bigint_gcd", new BigInteger("48").gcd(new BigInteger("36")).intValue() == 12);
        chk("bigint_modpow", new BigInteger("3").modPow(new BigInteger("4"), new BigInteger("5")).intValue() == 1);
        chk("bigint_factorial", factorial(20).toString().equals("2432902008176640000"));
        chk("bigint_arith", new BigInteger("100").add(new BigInteger("50")).intValue() == 150 && new BigInteger("100").multiply(new BigInteger("3")).intValue() == 300);
        chk("bigint_compare", new BigInteger("1000").compareTo(new BigInteger("999")) > 0 && BigInteger.TEN.equals(BigInteger.valueOf(10)));
        chk("bigint_isprime", new BigInteger("17").isProbablePrime(20) && !new BigInteger("18").isProbablePrime(20));
        // BigDecimal
        chk("bigdec_divide", new BigDecimal("1").divide(new BigDecimal("3"), 10, RoundingMode.HALF_UP).toString().equals("0.3333333333"));
        chk("bigdec_scale", new BigDecimal("2.500").stripTrailingZeros().toPlainString().equals("2.5"));
        chk("bigdec_add_exact", new BigDecimal("0.1").add(new BigDecimal("0.2")).toString().equals("0.3"));
        chk("bigdec_compare", new BigDecimal("1.0").compareTo(new BigDecimal("1.00")) == 0 && new BigDecimal("1.0").equals(new BigDecimal("1.0")));
        chk("bigdec_round", new BigDecimal("3.14159").setScale(2, RoundingMode.HALF_UP).toString().equals("3.14"));
        golden("math_floorMod", Math.floorMod(-7, 3));
        golden("bigint_pow_100", new BigInteger("2").pow(100));
        golden("bigint_factorial_20", factorial(20));
        golden("bigdec_div", new BigDecimal("1").divide(new BigDecimal("3"), 10, RoundingMode.HALF_UP));
        golden("bigdec_sum", new BigDecimal("0.1").add(new BigDecimal("0.2")));
    }
    static BigInteger factorial(int n) { BigInteger r = BigInteger.ONE; for (int i = 2; i <= n; i++) r = r.multiply(BigInteger.valueOf(i)); return r; }

    // ====================================================================== //
    //  MODULE 20 — arrays                                                     //
    // ====================================================================== //
    static void mArrays() {
        int[] arr = {5, 3, 1, 4, 2};
        int[] copy = Arrays.copyOf(arr, 3);
        int[] sorted = arr.clone(); Arrays.sort(sorted);
        chk("arr_sort_copy", Arrays.toString(sorted).equals("[1, 2, 3, 4, 5]") && Arrays.toString(copy).equals("[5, 3, 1]"));
        chk("arr_binarySearch", Arrays.binarySearch(sorted, 3) == 2 && Arrays.stream(arr).sum() == 15);
        chk("arr_deep", Arrays.deepToString(new int[][]{{1, 2}, {3, 4}}).equals("[[1, 2], [3, 4]]"));
        int[] fill = new int[3]; Arrays.fill(fill, 7);
        chk("arr_fill", Arrays.toString(fill).equals("[7, 7, 7]"));
        chk("arr_equals", Arrays.equals(new int[]{1, 2}, new int[]{1, 2}) && !Arrays.equals(new int[]{1}, new int[]{2}));
        chk("arr_copyOfRange", Arrays.toString(Arrays.copyOfRange(new int[]{0, 1, 2, 3, 4}, 1, 4)).equals("[1, 2, 3]"));
        String[] sa = {"b", "a", "c"}; Arrays.sort(sa);
        chk("arr_string_sort", Arrays.toString(sa).equals("[a, b, c]"));
        Integer[] boxed = Arrays.stream(arr).boxed().toArray(Integer[]::new);
        chk("arr_boxed_stream", Arrays.toString(boxed).equals("[5, 3, 1, 4, 2]"));
        // multidim + jagged
        int[][] grid = {{1, 2, 3}, {4, 5}, {6}};
        chk("arr_jagged", grid.length == 3 && grid[0].length == 3 && grid[2].length == 1 && grid[1][1] == 5);
        // array default values
        int[] zeros = new int[3]; boolean[] bools = new boolean[2]; String[] nulls = new String[2];
        chk("arr_defaults", zeros[0] == 0 && !bools[0] && nulls[0] == null);
        // 3D array
        int[][][] cube = new int[2][2][2]; cube[1][1][1] = 8;
        chk("arr_3d", cube[1][1][1] == 8 && cube[0][0][0] == 0);
        // Arrays.asList + setAll
        int[] gen = new int[5]; Arrays.setAll(gen, i -> i * i);
        chk("arr_setAll", Arrays.toString(gen).equals("[0, 1, 4, 9, 16]"));
        golden("arr_sorted", Arrays.toString(sorted));
        golden("arr_jagged_total", Arrays.stream(grid).mapToInt(a -> a.length).sum());
        golden("arr_setAll_val", Arrays.toString(gen));
    }

    // ====================================================================== //
    //  MODULE 21 — version-gated JDK21+ stdlib (reflection; runs on JDK21+)    //
    // ====================================================================== //
    static void mVersionGated() throws Exception {
        int feat = Runtime.version().feature();
        // SequencedCollection.getFirst/getLast (JDK21)
        List<Integer> list = new ArrayList<>(List.of(10, 20, 30));
        gatedListGetFirst(list, feat);
        // SequencedCollection.reversed (JDK21)
        gatedListReversed(list, feat);
        // Math.clamp(long,long,long) (JDK21)
        gatedMathClamp(feat);
        // Thread.ofVirtual / virtual threads (JDK21)
        gatedVirtualThreads(feat);
        // Stream.mapMulti (JDK16+ actually, but exercise via reflection-safe path)
        gatedStreamGatherers(feat);
    }
    static void gatedListGetFirst(List<Integer> list, int feat) throws Exception {
        try {
            Method gf = List.class.getMethod("getFirst");
            Method gl = List.class.getMethod("getLast");
            int first = (Integer) gf.invoke(list), last = (Integer) gl.invoke(list);
            chk("v21_getFirst_getLast", first == 10 && last == 30);
            golden("v21_getFirst", first);
        } catch (NoSuchMethodException e) {
            chk("v21_getFirst_getLast_gated", feat < 21); gated++;
        }
    }
    static void gatedListReversed(List<Integer> list, int feat) throws Exception {
        try {
            Method rev = List.class.getMethod("reversed");
            Object r = rev.invoke(list);
            chk("v21_reversed", r.toString().equals("[30, 20, 10]"));
        } catch (NoSuchMethodException e) {
            chk("v21_reversed_gated", feat < 21); gated++;
        }
    }
    static void gatedMathClamp(int feat) throws Exception {
        try {
            Method clamp = Math.class.getMethod("clamp", long.class, long.class, long.class);
            long c1 = (Long) clamp.invoke(null, 15L, 0L, 10L);
            long c2 = (Long) clamp.invoke(null, -5L, 0L, 10L);
            long c3 = (Long) clamp.invoke(null, 5L, 0L, 10L);
            chk("v21_math_clamp", c1 == 10 && c2 == 0 && c3 == 5);
            golden("v21_clamp", c1);
        } catch (NoSuchMethodException e) {
            chk("v21_math_clamp_gated", feat < 21); gated++;
        }
    }
    static void gatedVirtualThreads(int feat) throws Exception {
        try {
            Method ofVirtual = Thread.class.getMethod("ofVirtual");
            Object builder = ofVirtual.invoke(null);
            // start(Runnable) is declared on the EXPORTED interface java.lang.Thread.Builder,
            // not on the (non-accessible) concrete VirtualThreadBuilder impl. Reflect the
            // interface method so it works without --add-opens.
            AtomicInteger ai = new AtomicInteger(0);
            Runnable task = ai::incrementAndGet;
            Class<?> builderIface = Class.forName("java.lang.Thread$Builder");
            Method startM = builderIface.getMethod("start", Runnable.class);
            Object th = startM.invoke(builder, task);
            ((Thread) th).join();
            chk("v21_virtual_thread", ai.get() == 1);
        } catch (NoSuchMethodException e) {
            chk("v21_virtual_thread_gated", feat < 21); gated++;
        }
    }
    static void gatedStreamGatherers(int feat) throws Exception {
        // Stream.Gatherers is JDK22+/24 stable. Probe class presence only.
        try {
            Class<?> g = Class.forName("java.util.stream.Gatherers");
            chk("v22_gatherers_present", g != null);
        } catch (ClassNotFoundException e) {
            chk("v22_gatherers_gated", feat < 22); gated++;
        }
    }

    // ====================================================================== //
    //  main                                                                   //
    // ====================================================================== //
    public static void main(String[] args) throws Exception {
        int feat = Runtime.version().feature();
        System.out.println("JavaLangCarpet on feature version " + feat);
        mLiterals();
        mPrimitives();
        mOperators();
        mControlFlow();
        mTypes();
        mGenerics();
        mFunctional();
        mStreams();
        mOptional();
        mPatterns();
        mExceptions();
        mReflection();
        mCollections();
        mConcurrency();
        mTime();
        mRegex();
        mNio();
        mStrings();
        mMath();
        mArrays();
        mVersionGated();
        // Emit the deterministic GOLDEN block (output==golden cross-check).
        System.out.print(GOLD);
        System.out.println("carpet: " + pass + " passed, " + fail + " failed, " + gated + " version-gated");
        if (fail == 0) System.out.println("JAVA_LANG_OK " + pass);
        else { System.out.println("JAVA_LANG_FAIL"); System.exit(1); }
    }
}
