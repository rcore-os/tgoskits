// Jdk23Features.java — JDK 23 language/stdlib feature self-test.
// Run via single-file source mode WITH preview enabled (23's headline features
// are preview, so this is the honest way to exercise them):
//     java --enable-preview --source 23 Jdk23Features.java
// Prints "JDK23_OK" only if every assertion passed AND the running JVM is 23.
//
// Features exercised:
//   STABLE (final in 23, no preview needed; run anyway under the same JVM):
//     * records + nested record patterns in switch (final since 21)
//     * Stream.mapMulti (final since 16)  — flatten-ish stable stream op
//   PREVIEW in 23 (gated by --enable-preview --source 23):
//     * Flexible Constructor Bodies (JEP 482, 2nd preview) — statements before super()
//     * Stream Gatherers (JEP 473, 2nd preview) — Stream::gather + Gatherers.windowFixed/fold
import java.util.ArrayList;
import java.util.List;
import java.util.stream.Gatherers;
import java.util.stream.Stream;

public class Jdk23Features {

    // --- PREVIEW: Flexible Constructor Bodies (statements before super()) ---
    static class Base {
        final int v;
        Base(int v) { this.v = v; }
    }
    static class Validated extends Base {
        final String label;
        Validated(int x, String label) {
            // prologue runs BEFORE super() — validate + transform args
            if (x < 0) throw new IllegalArgumentException("must be >= 0");
            String l = label == null ? "anon" : label.trim();
            int doubled = x * 2;
            super(doubled);           // super() no longer required to be first
            this.label = l;           // epilogue after super()
        }
    }

    // --- STABLE: nested record patterns (final since 21) ---
    record Pair(int a, int b) {}
    static int sumPair(Object o) {
        return switch (o) {
            case Pair(int a, int b) -> a + b;
            default -> -1;
        };
    }

    static void check(boolean cond, String what) {
        if (!cond) throw new AssertionError("JDK23 FAIL: " + what);
    }

    public static void main(String[] args) {
        int major = Runtime.version().feature();
        System.out.println("running feature version = " + major);
        check(major == 23, "expected JVM feature version 23, got " + major);

        // PREVIEW: flexible constructor bodies
        Validated ok = new Validated(21, "  hello  ");
        check(ok.v == 42, "flexible-ctor prologue computed super arg");
        check(ok.label.equals("hello"), "flexible-ctor epilogue set field");
        boolean threw = false;
        try { new Validated(-1, "x"); } catch (IllegalArgumentException e) { threw = true; }
        check(threw, "flexible-ctor prologue validation rejects bad arg before super()");

        // PREVIEW: Stream Gatherers — fixed windows
        List<List<Integer>> windows = Stream.of(1, 2, 3, 4, 5)
                .gather(Gatherers.windowFixed(2))
                .toList();
        check(windows.equals(List.of(List.of(1, 2), List.of(3, 4), List.of(5))), "Gatherers.windowFixed");
        // PREVIEW: Gatherers.fold — running fold to a single accumulated value
        List<Integer> folded = Stream.of(1, 2, 3, 4)
                .gather(Gatherers.fold(() -> 0, Integer::sum))
                .toList();
        check(folded.equals(List.of(10)), "Gatherers.fold sum");

        // STABLE: nested record pattern in switch
        check(sumPair(new Pair(3, 4)) == 7, "record pattern switch");
        check(sumPair("nope") == -1, "record pattern default");

        // STABLE: Stream.mapMulti (final since 16) — expand each n into n copies
        List<Integer> expanded = new ArrayList<>();
        Stream.of(1, 2, 3).<Integer>mapMulti((n, consumer) -> {
            for (int i = 0; i < n; i++) consumer.accept(n);
        }).forEach(expanded::add);
        check(expanded.equals(List.of(1, 2, 2, 3, 3, 3)), "Stream.mapMulti");

        System.out.println("JDK23_OK");
    }
}
