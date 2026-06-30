// Jdk17Features.java — JDK 17 (LTS) language/stdlib feature self-test.
// Run via single-file source mode:  java Jdk17Features.java
// Prints "JDK17_OK" on the last line ONLY if every assertion passed AND the
// running JVM reports major version 17. Any failure throws -> no JDK17_OK.
//
// Features exercised (all STABLE/final in 17):
//   * records (JEP 395)
//   * sealed classes/interfaces (JEP 409)
//   * pattern matching for instanceof (JEP 394)
//   * text blocks (JEP 378)
//   * switch expressions with arrow + yield (JEP 361)
//   * Stream.toList() (added in 16, stable in 17)
import java.util.List;
import java.util.stream.Stream;

public class Jdk17Features {
    // --- records ---
    record Point(int x, int y) {
        int manhattan() { return Math.abs(x) + Math.abs(y); }
    }

    // --- sealed interface + permitted record/finals implementations ---
    sealed interface Shape permits Circle, Square {}
    record Circle(double r) implements Shape {}
    record Square(double side) implements Shape {}

    static double area(Shape s) {
        // switch expression (arrow form) + pattern-matching-ish dispatch via instanceof
        if (s instanceof Circle c) {            // pattern matching for instanceof binds `c`
            return Math.PI * c.r() * c.r();
        } else if (s instanceof Square sq) {
            return sq.side() * sq.side();
        }
        throw new IllegalStateException("unreachable (sealed)");
    }

    enum Day { MON, TUE, SAT, SUN }

    static String kind(Day d) {
        // switch expression returning a value, with yield in a block arm
        return switch (d) {
            case SAT, SUN -> "weekend";
            case MON, TUE -> {
                String s = "week";
                yield s + "day";
            }
        };
    }

    static void check(boolean cond, String what) {
        if (!cond) throw new AssertionError("JDK17 FAIL: " + what);
    }

    public static void main(String[] args) {
        // version red-line: must actually be running on JDK 17.
        int major = Runtime.version().feature();
        System.out.println("running feature version = " + major);
        check(major == 17, "expected JVM feature version 17, got " + major);

        // records: components, equals, hashCode, accessors
        Point p1 = new Point(3, -4);
        Point p2 = new Point(3, -4);
        check(p1.equals(p2), "record equals");
        check(p1.hashCode() == p2.hashCode(), "record hashCode");
        check(p1.x() == 3 && p1.y() == -4, "record accessors");
        check(p1.manhattan() == 7, "record method");
        check(p1.toString().equals("Point[x=3, y=-4]"), "record toString");

        // sealed + pattern matching for instanceof
        check(Math.abs(area(new Square(3)) - 9.0) < 1e-9, "sealed Square area");
        check(area(new Circle(1)) > 3.14 && area(new Circle(1)) < 3.15, "sealed Circle area");

        // switch expression
        check(kind(Day.SAT).equals("weekend"), "switch weekend");
        check(kind(Day.MON).equals("weekday"), "switch weekday yield");

        // text block (JEP 378) — incidental whitespace stripped, \n line endings
        String tb = """
                line1
                line2
                """;
        check(tb.equals("line1\nline2\n"), "text block content");

        // Stream.toList() — returns an unmodifiable list
        List<Integer> evens = Stream.of(1, 2, 3, 4, 5, 6)
                .filter(n -> n % 2 == 0)
                .map(n -> n * n)
                .toList();
        check(evens.equals(List.of(4, 16, 36)), "Stream.toList content");
        boolean immutable;
        try { evens.add(99); immutable = false; } catch (UnsupportedOperationException e) { immutable = true; }
        check(immutable, "Stream.toList immutable");

        System.out.println("JDK17_OK");
    }
}
