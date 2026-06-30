// BackCompat.java — Java backward-compatibility test for the openjdk-multi case (#764).
// Compiled ONCE with javac --release 17 (class file version 61), then run UNCHANGED on
// JDK 17 / 21 / 23 / 25. A newer JVM must run older bytecode and produce byte-identical
// output (the JVM backward-compatibility guarantee). Uses only JDK17-level language +
// stdlib so the SAME .class is valid on every target JDK. Deterministic output.
import java.util.*;
import java.util.stream.*;

public class BackCompat {
    // JDK16/17 record
    record Point(int x, int y) { int sum() { return x + y; } }
    // JDK17 sealed
    sealed interface Shape permits Circle, Square {}
    record Circle(int r) implements Shape {}
    record Square(int s) implements Shape {}
    static int area(Shape sh) {
        // JDK16/17 instanceof patterns (pattern-switch is JDK21+, so use if-else to stay 17-clean)
        if (sh instanceof Circle c) return 3 * c.r() * c.r();
        if (sh instanceof Square q) return q.s() * q.s();
        return -1;
    }

    public static void main(String[] a) {
        var out = new StringBuilder();
        // record
        out.append("REC=").append(new Point(3, 4).sum()).append(';');
        // sealed + switch-expr
        out.append("AREA=").append(area(new Circle(2)) + area(new Square(3))).append(';');
        // text block (JDK15+)
        String tb = """
            line1
            line2""";
        out.append("TB=").append(tb.lines().count()).append(';');
        // instanceof pattern (JDK16+)
        Object o = "hello";
        out.append("PAT=").append(o instanceof String s ? s.length() : -1).append(';');
        // Stream.toList (JDK16+)
        out.append("STREAM=").append(Stream.of(1, 2, 3, 4).filter(x -> x % 2 == 0).toList()).append(';');
        // var + collections + math
        var m = new TreeMap<String, Integer>();
        m.put("b", 2); m.put("a", 1);
        out.append("MAP=").append(m).append(';');
        // deterministic, version-independent
        System.out.println("BACKCOMPAT_RUN=" + out);
    }
}
