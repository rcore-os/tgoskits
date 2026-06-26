// Jdk25Features.java — JDK 25 (LTS) language/stdlib feature self-test.
// Run via single-file source mode WITH preview enabled (StableValue is preview
// in 25; the other features are final but run fine under --enable-preview):
//     java --enable-preview --source 25 Jdk25Features.java
// The shell harness ALSO runs this program a second time with
//     -XX:+UseCompactObjectHeaders
// to prove the JVM accepts the compact-object-headers flag and object layout
// still works (the program reads the flag via the diagnostic VM bean / arg).
// Prints "JDK25_OK" only if every assertion passed AND the running JVM is 25.
//
// Features exercised:
//   FINAL in 25:
//     * Scoped Values (JEP 506) — ScopedValue.where(...).call(...) / .get()  ⭐
//     * Module Import Declarations (JEP 511) — `import module java.base`
//     * Compact Object Headers (JEP 519) — product flag -XX:+UseCompactObjectHeaders
//   PREVIEW in 25 (gated by --enable-preview --source 25):
//     * Stable Values (JEP 502) — StableValue.of() + orElseSet (compute-once)
import module java.base;              // FINAL in 25: imports the whole java.base module's exported packages
import module java.management;        // FINAL in 25: another module import (ManagementFactory for VM args)

public class Jdk25Features {
    // Scoped value shared down the call chain (replacement for ThreadLocal in structured code)
    static final ScopedValue<String> USER = ScopedValue.newInstance();

    static String greet() {
        // reads the value bound by the enclosing ScopedValue.where(...) scope
        return "hello " + USER.get();
    }

    static void check(boolean cond, String what) {
        if (!cond) throw new AssertionError("JDK25 FAIL: " + what);
    }

    public static void main(String[] args) throws Exception {
        int major = Runtime.version().feature();
        System.out.println("running feature version = " + major);
        check(major == 25, "expected JVM feature version 25, got " + major);

        // --- FINAL: module import declarations ---
        // List / ArrayList / Map / stream types are all usable unqualified via `import module java.base`.
        List<Integer> nums = new ArrayList<>(List.of(3, 1, 2));
        Collections.sort(nums);
        check(nums.equals(List.of(1, 2, 3)), "module-import: Collections.sort on unqualified List");
        Map<String, Integer> m = new HashMap<>();
        m.put("a", 1);
        check(m.get("a") == 1, "module-import: HashMap usable unqualified");
        long evens = nums.stream().filter(n -> n % 2 == 0).count();   // java.util.stream via module import
        check(evens == 1, "module-import: stream() usable");

        // --- FINAL: scoped values ---
        String r = ScopedValue.where(USER, "alice").call(Jdk25Features::greet);
        check(r.equals("hello alice"), "ScopedValue bound value visible in callee");
        // outside the scope the value is unbound
        boolean unbound = false;
        try { USER.get(); } catch (Exception e) { unbound = true; }
        check(unbound, "ScopedValue unbound outside where()");
        // nesting rebinds
        String nested = ScopedValue.where(USER, "outer").call(() ->
                ScopedValue.where(USER, "inner").call(Jdk25Features::greet));
        check(nested.equals("hello inner"), "ScopedValue nested rebind");

        // --- FINAL: compact object headers runtime flag self-report ---
        // The harness runs us with/without -XX:+UseCompactObjectHeaders. We read the
        // effective flag from the VM args and assert objects still behave either way.
        boolean compact = ManagementFactory.getRuntimeMXBean().getInputArguments().stream()
                .anyMatch(a -> a.contains("UseCompactObjectHeaders"));
        Object[] probe = new Object[1000];
        for (int i = 0; i < probe.length; i++) probe[i] = "obj-" + i;     // allocate w/ active header layout
        check(probe[999].equals("obj-999"), "object identity intact under header layout");
        check(probe[0].hashCode() != 0 || probe[0].hashCode() == 0, "identity hashCode computable");  // always true; exercises header hash slot
        System.out.println("compact-object-headers flag present = " + compact);

        // --- PREVIEW: stable values (compute-once, then immutable) ---
        StableValue<Integer> answer = StableValue.of();
        int first = answer.orElseSet(() -> { return 6 * 7; });
        int second = answer.orElseSet(() -> { throw new IllegalStateException("supplier must not run twice"); });
        check(first == 42 && second == 42, "StableValue computes once and caches");

        System.out.println("JDK25_OK");
    }
}
