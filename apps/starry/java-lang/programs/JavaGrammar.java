// JavaGrammar.java — full-JLS language-grammar carpet for StarryOS java-lang (#764).
// Exercises the Java language grammar item-by-item (literals, operators, control
// flow, type declarations, generics, functional, exceptions, concurrency,
// reflection, initializers, arrays). Stable features through JDK 17 (the LTS
// baseline); version-specific deltas (records/sealed in 17, virtual threads in
// 21, etc.) are additionally covered by Jdk{17,21,23,25}Features.java.
//
// Run:  java JavaGrammar.java   (single-file source mode)
// Prints JAVA_GRAMMAR_OK on the last line iff every assertion passed.
import java.util.*;
import java.util.concurrent.*;
import java.util.concurrent.atomic.*;
import java.util.function.*;
import java.util.stream.*;
import java.lang.annotation.*;
import java.lang.reflect.*;

public class JavaGrammar {
    static int pass = 0, fail = 0;
    static void chk(String name, boolean cond) {
        if (cond) { pass++; }
        else { fail++; System.out.println("  FAIL " + name); }
    }

    // ---- annotations (declaration, retention, repeatable, type annotation) ----
    @Retention(RetentionPolicy.RUNTIME) @interface Tag { String value(); }
    @Retention(RetentionPolicy.RUNTIME) @interface Tags { Mark[] value(); }
    @Retention(RetentionPolicy.RUNTIME) @Repeatable(Tags.class) @interface Mark { String value(); }
    @Tag("cls") static class Annotated {}
    @Mark("a") @Mark("b") static class Repeated {}

    // ---- generics: bounded type param + wildcards + generic method ----
    static <T extends Comparable<T>> T max(List<T> xs) {
        T m = xs.get(0);
        for (T x : xs) if (x.compareTo(m) > 0) m = x;
        return m;
    }
    static double sum(List<? extends Number> xs) {
        double s = 0; for (Number n : xs) s += n.doubleValue(); return s;
    }

    // ---- enum with body + abstract method ----
    enum Op {
        ADD { int apply(int a, int b) { return a + b; } },
        MUL { int apply(int a, int b) { return a * b; } };
        abstract int apply(int a, int b);
    }

    // ---- record + compact constructor ----
    record Frac(int num, int den) {
        Frac { if (den == 0) throw new IllegalArgumentException("den=0"); }
        double val() { return (double) num / den; }
    }

    // ---- sealed hierarchy ----
    sealed interface Node permits Leaf, Branch {}
    record Leaf(int v) implements Node {}
    record Branch(Node l, Node r) implements Node {}
    static int total(Node n) {
        if (n instanceof Leaf lf) return lf.v();
        if (n instanceof Branch b) return total(b.l()) + total(b.r());
        throw new IllegalStateException();
    }

    // ---- interface: default / static / private methods ----
    interface Greeter {
        String name();
        default String hi() { return prefix() + name(); }
        static Greeter of(String n) { return () -> n; }
        private String prefix() { return "hi "; }
    }

    // ---- custom exception + AutoCloseable for try-with-resources ----
    static class Res implements AutoCloseable {
        final StringBuilder log; Res(StringBuilder l){ log = l; log.append("open;"); }
        public void close(){ log.append("close;"); }
    }

    // ---- static + instance initializers ----
    static int sinit; static { sinit = 42; }
    int iinit; { iinit = 7; }

    static void literals() {
        int dec = 1_000_000, hex = 0xFF, oct = 0_17, bin = 0b1010;
        long big = 9_000_000_000L;
        double d = 1.5e3, hexf = 0x1.8p1;   // hex float = 3.0
        float f = 2.5f;
        char uni = 'A';                // 'A'
        boolean t = true;
        String s = "a\tb";
        String block = """
            line1
            line2""";
        chk("lit_underscore_dec", dec == 1000000);
        chk("lit_hex", hex == 255);
        chk("lit_octal", oct == 15);
        chk("lit_binary", bin == 10);
        chk("lit_long", big == 9000000000L);
        chk("lit_double_exp", d == 1500.0);
        chk("lit_hex_float", hexf == 3.0);
        chk("lit_float", f == 2.5f);
        chk("lit_char_unicode", uni == 'A');
        chk("lit_bool", t);
        chk("lit_string_escape", s.length() == 3 && s.charAt(1) == '\t');
        chk("lit_text_block", block.equals("line1\nline2"));
        chk("lit_null", ((Object) null) == null);
    }

    static void operators() {
        chk("op_arith", 7 / 2 == 3 && 7 % 2 == 1 && 2 * 3 + 1 == 7);
        chk("op_bitwise", (0b1100 & 0b1010) == 0b1000 && (0b1100 | 0b1010) == 0b1110
                && (0b1100 ^ 0b1010) == 0b0110 && (~0) == -1);
        chk("op_shift", (1 << 4) == 16 && (-8 >> 1) == -4 && (-1 >>> 28) == 15);
        chk("op_relational", 1 < 2 && 2 <= 2 && 3 > 2 && 3 >= 3 && 1 != 2 && 2 == 2);
        chk("op_logical", (true && true) && (false || true) && !false);
        int x = 5;
        x += 3;    // 8
        x -= 1;    // 7
        x *= 3;    // 21
        x /= 2;    // 10
        x %= 7;    // 3
        x <<= 2;   // 12
        x >>= 1;   // 6
        x &= 0xE;  // 6
        x |= 1;    // 7
        x ^= 2;    // 5
        chk("op_compound_assign", x == 5);
        chk("op_ternary", (3 > 2 ? "y" : "n").equals("y"));
        Object o = "str";
        chk("op_instanceof", o instanceof String);
        int a = 1; int b = a++ + ++a;   // 1 + 3
        chk("op_inc_dec", a == 3 && b == 4);
    }

    static void controlFlow() {
        int sum = 0; for (int i = 0; i < 5; i++) sum += i;
        chk("ctrl_for", sum == 10);
        int n = 0; while (n < 3) n++;
        chk("ctrl_while", n == 3);
        int m = 0; do { m++; } while (m < 2);
        chk("ctrl_do_while", m == 2);
        int es = 0; for (int v : new int[]{1, 2, 3}) es += v;
        chk("ctrl_enhanced_for", es == 6);
        String r; switch (2) { case 1: r = "a"; break; case 2: r = "b"; break; default: r = "z"; }
        chk("ctrl_switch_stmt", r.equals("b"));
        int code = switch (3) { case 1, 2 -> 10; case 3 -> { int y = 30; yield y; } default -> 0; };
        chk("ctrl_switch_expr_yield", code == 30);
        int found = -1;
        outer: for (int i = 0; i < 3; i++) for (int j = 0; j < 3; j++) if (i + j == 3) { found = i * 10 + j; break outer; }
        chk("ctrl_labeled_break", found == 12);
    }

    static void exceptions() {
        StringBuilder log = new StringBuilder();
        try (Res a = new Res(log); Res b = new Res(log)) { log.append("body;"); }
        chk("exc_try_with_resources", log.toString().equals("open;open;body;close;close;"));
        String caught = "";
        try { throw new NumberFormatException("x"); }
        catch (IllegalStateException | NumberFormatException e) { caught = e.getClass().getSimpleName(); }
        chk("exc_multi_catch", caught.equals("NumberFormatException"));
        boolean fin = false;
        try { int z = 1 / 0; } catch (ArithmeticException e) { } finally { fin = true; }
        chk("exc_finally", fin);
    }

    static void genericsFunctional() {
        chk("gen_bounded_method", max(List.of(3, 9, 4)) == 9);
        chk("gen_wildcard", sum(List.of(1, 2, 3.5)) == 6.5);
        List<Integer> xs = List.of(1, 2, 3, 4, 5);
        int evens = (int) xs.stream().filter(i -> i % 2 == 0).count();
        chk("stream_filter", evens == 2);
        List<Integer> doubled = xs.stream().map(i -> i * 2).collect(Collectors.toList());
        chk("stream_map_collect", doubled.equals(List.of(2, 4, 6, 8, 10)));
        int red = xs.stream().reduce(0, Integer::sum);
        chk("stream_reduce_methodref", red == 15);
        List<Integer> tl = xs.stream().limit(2).toList();
        chk("stream_toList", tl.equals(List.of(1, 2)));
        Optional<Integer> first = xs.stream().filter(i -> i > 3).findFirst();
        chk("optional", first.isPresent() && first.get() == 4);
        Function<Integer, Integer> inc = i -> i + 1;
        BiFunction<Integer, Integer, Integer> add = Integer::sum;
        Supplier<String> sup = String::new;
        chk("lambda_methodref_ctor", inc.apply(4) == 5 && add.apply(2, 3) == 5 && sup.get().isEmpty());
        Runnable rr = () -> {};
        chk("functional_iface", rr != null);
    }

    static void typesAndNesting() {
        chk("record_compact_ctor", new Frac(1, 2).val() == 0.5);
        boolean threw = false;
        try { new Frac(1, 0); } catch (IllegalArgumentException e) { threw = true; }
        chk("record_compact_validate", threw);
        chk("sealed_hierarchy", total(new Branch(new Leaf(3), new Branch(new Leaf(4), new Leaf(5)))) == 12);
        chk("enum_abstract_body", Op.ADD.apply(2, 3) == 5 && Op.MUL.apply(2, 3) == 6);
        chk("interface_default_static_private", Greeter.of("sky").hi().equals("hi sky"));
        // anonymous class
        Greeter anon = new Greeter() { public String name() { return "anon"; } };
        chk("anonymous_class", anon.hi().equals("hi anon"));
        // local class
        class Local { int v() { return 99; } }
        chk("local_class", new Local().v() == 99);
        // var + autoboxing
        var list = new ArrayList<Integer>(); list.add(5);
        int unboxed = list.get(0);
        chk("var_and_autoboxing", unboxed == 5);
        // varargs
        chk("varargs", varsum(1, 2, 3, 4) == 10);
        // multidim array + initializer
        int[][] grid = {{1, 2}, {3, 4}};
        chk("array_multidim_init", grid[1][1] == 4 && grid.length == 2);
        // initializers ran
        chk("static_initializer", sinit == 42);
        chk("instance_initializer", new JavaGrammar().iinit == 7);
    }
    static int varsum(int... xs) { int s = 0; for (int x : xs) s += x; return s; }

    static void annotationsReflection() throws Exception {
        Tag tg = Annotated.class.getAnnotation(Tag.class);
        chk("annotation_runtime", tg != null && tg.value().equals("cls"));
        Mark[] marks = Repeated.class.getAnnotationsByType(Mark.class);
        chk("annotation_repeatable", marks.length == 2 && marks[0].value().equals("a"));
        Method mm = JavaGrammar.class.getDeclaredMethod("varsum", int[].class);
        chk("reflection_method", mm.getReturnType() == int.class);
        Object res = mm.invoke(null, (Object) new int[]{2, 3});
        chk("reflection_invoke", ((Integer) res) == 5);
        Class<?> c = Class.forName("java.lang.String");
        chk("reflection_forname", c.getSimpleName().equals("String"));
    }

    static void concurrency() throws Exception {
        AtomicInteger counter = new AtomicInteger(0);
        int N = 8;
        CountDownLatch latch = new CountDownLatch(N);
        ExecutorService ex = Executors.newFixedThreadPool(4);
        for (int i = 0; i < N; i++) ex.submit(() -> { counter.incrementAndGet(); latch.countDown(); });
        latch.await(30, TimeUnit.SECONDS);
        ex.shutdown();
        chk("concurrency_executor_atomic", counter.get() == N);
        CompletableFuture<Integer> cf = CompletableFuture.supplyAsync(() -> 20).thenApply(v -> v + 1);
        chk("concurrency_completablefuture", cf.get(30, TimeUnit.SECONDS) == 21);
        // synchronized + volatile smoke
        final Object lock = new Object();
        int[] acc = {0};
        Thread th = new Thread(() -> { synchronized (lock) { acc[0]++; } });
        th.start(); th.join();
        chk("concurrency_thread_synchronized", acc[0] == 1);
    }

    public static void main(String[] args) throws Exception {
        int major = Runtime.version().feature();
        System.out.println("java grammar carpet on feature version " + major);
        literals();
        operators();
        controlFlow();
        exceptions();
        genericsFunctional();
        typesAndNesting();
        annotationsReflection();
        concurrency();
        System.out.println("grammar: " + pass + " passed, " + fail + " failed");
        System.out.println(fail == 0 ? "JAVA_GRAMMAR_OK" : "JAVA_GRAMMAR_FAIL");
        if (fail != 0) System.exit(1);
    }
}
