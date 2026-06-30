// Jdk21Features.java — JDK 21 (LTS) language/stdlib feature self-test.
// Run via single-file source mode:  java Jdk21Features.java
// Prints "JDK21_OK" only if every assertion passed AND the running JVM is 21.
//
// Features exercised (all FINAL in 21, no --enable-preview needed):
//   * virtual threads (JEP 444) — Thread.ofVirtual + newVirtualThreadPerTaskExecutor  ⭐
//   * record patterns (JEP 440) — deconstruction in instanceof + switch
//   * pattern matching for switch (JEP 441) incl. guarded patterns (when)
//   * sequenced collections (JEP 431) — SequencedCollection / getFirst / getLast / reversed
//   * Math.clamp (added in 21)
import java.util.ArrayList;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.SequencedCollection;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.Future;
import java.util.concurrent.atomic.AtomicLong;

public class Jdk21Features {
    record Point(int x, int y) {}
    sealed interface Shape permits Box, Dot {}
    record Box(Point lo, Point hi) implements Shape {}
    record Dot(Point at) implements Shape {}

    // record patterns + pattern matching for switch + guarded patterns
    static String describe(Object o) {
        return switch (o) {
            case Box(Point(var x1, var y1), Point(var x2, var y2)) when (x2 - x1) == (y2 - y1) ->
                    "square-box(" + (x2 - x1) + ")";
            case Box(Point lo, Point hi) -> "box(" + lo.x() + "," + lo.y() + "->" + hi.x() + "," + hi.y() + ")";
            case Dot(Point(var x, var y)) -> "dot(" + x + "," + y + ")";
            case Integer i when i < 0 -> "neg-int";
            case Integer i -> "int(" + i + ")";
            case null -> "null";
            default -> "other";
        };
    }

    static void check(boolean cond, String what) {
        if (!cond) throw new AssertionError("JDK21 FAIL: " + what);
    }

    public static void main(String[] args) throws Exception {
        int major = Runtime.version().feature();
        System.out.println("running feature version = " + major);
        check(major == 21, "expected JVM feature version 21, got " + major);

        // --- virtual threads via Thread.ofVirtual ---
        AtomicLong counter = new AtomicLong();
        Thread vt = Thread.ofVirtual().name("vt-probe").start(() -> counter.incrementAndGet());
        vt.join();
        check(vt.isVirtual(), "Thread.ofVirtual is virtual");
        check(counter.get() == 1, "virtual thread ran once");

        // --- virtual thread per-task executor: fan out 1000 tasks ---
        long expected = 0;
        try (ExecutorService ex = Executors.newVirtualThreadPerTaskExecutor()) {
            List<Future<Integer>> fs = new ArrayList<>();
            for (int i = 1; i <= 1000; i++) {
                final int n = i;
                fs.add(ex.submit(() -> n));
            }
            long got = 0;
            for (Future<Integer> f : fs) got += f.get();
            expected = 1000L * 1001L / 2L;
            check(got == expected, "vthread executor sum (" + got + " != " + expected + ")");
        }
        System.out.println("vthreads summed 1..1000 = " + expected);

        // --- record patterns + guarded switch ---
        check(describe(new Box(new Point(0, 0), new Point(5, 5))).equals("square-box(5)"), "guarded square-box");
        check(describe(new Box(new Point(0, 0), new Point(5, 2))).equals("box(0,0->5,2)"), "record-pattern box");
        check(describe(new Dot(new Point(7, 9))).equals("dot(7,9)"), "nested record pattern dot");
        check(describe(-3).equals("neg-int"), "guarded neg int");
        check(describe(42).equals("int(42)"), "int pattern");
        check(describe(null).equals("null"), "null case label");
        check(describe("hi").equals("other"), "default case");

        // --- sequenced collections ---
        SequencedCollection<String> sc = new ArrayList<>(List.of("a", "b", "c"));
        check(sc.getFirst().equals("a"), "SequencedCollection getFirst");
        check(sc.getLast().equals("c"), "SequencedCollection getLast");
        sc.addFirst("z");
        sc.addLast("y");
        check(sc.getFirst().equals("z") && sc.getLast().equals("y"), "addFirst/addLast");
        check(sc.reversed().getFirst().equals("y"), "reversed view");
        LinkedHashSet<Integer> ls = new LinkedHashSet<>(List.of(10, 20, 30));
        check(ls.getFirst() == 10 && ls.getLast() == 30, "SequencedSet first/last");

        // --- Math.clamp ---
        check(Math.clamp(15, 0, 10) == 10, "clamp high");
        check(Math.clamp(-5, 0, 10) == 0, "clamp low");
        check(Math.clamp(7, 0, 10) == 7, "clamp mid");
        check(Math.clamp(3.5, 0.0, 1.0) == 1.0, "clamp double");

        System.out.println("JDK21_OK");
    }
}
