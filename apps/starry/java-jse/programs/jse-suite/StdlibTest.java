import java.util.*;
import java.util.stream.*;
import java.util.function.*;
import java.lang.annotation.*;
import java.lang.reflect.*;

/*
 * StdlibTest — JSE standard-library CARPET (default package, deterministic, offline, pure-JDK17).
 *
 * Lane within the jse-suite: the java.util collections framework + java.util.stream +
 * Collectors + java.util.function + Comparator + Optional + generics + annotations/reflection +
 * the modern language stdlib surface (records / sealed / pattern instanceof / switch expressions /
 * text blocks / var) + Objects/Arrays/Collections/StringJoiner utilities. Every assertion is an
 * exact equality / identity / thrown-exception check on self-fabricated data — no network, no
 * filesystem, no JIT dependence, no large heap/arrays/threads. Iteration-order assertions only use
 * ordered containers (List / LinkedHashMap / TreeMap / EnumSet) — never raw HashMap/HashSet order.
 */
public class StdlibTest {
    static int ok = 0, fail = 0;

    static void check(boolean c, String n) { if (c) ok++; else { fail++; System.out.println("FAIL " + n); } }
    static void eq(Object a, Object b, String n) {
        if (Objects.equals(a, b)) ok++;
        else { fail++; System.out.println("FAIL " + n + " exp=[" + b + "] got=[" + a + "]"); }
    }
    static void closeEq(double a, double b, String n) {
        if (Math.abs(a - b) < 1e-9) ok++;
        else { fail++; System.out.println("FAIL " + n + " exp=[" + b + "] got=[" + a + "]"); }
    }
    interface Run { void run() throws Throwable; }
    static void throwsEx(Class<? extends Throwable> ex, Run r, String n) {
        try {
            r.run();
        } catch (Throwable t) {
            if (ex.isInstance(t)) { ok++; return; }
            fail++; System.out.println("FAIL " + n + " wrong-ex=" + t.getClass().getName());
            return;
        }
        fail++; System.out.println("FAIL " + n + " no-throw");
    }
    static void section(Run r, String n) {
        try { r.run(); }
        catch (Throwable t) { fail++; System.out.println("FAIL section-" + n + " " + t.getClass().getName() + ":" + t.getMessage()); }
    }

    // ---- nested types for generics / annotations / reflection / records / sealed / enum ----
    @Retention(RetentionPolicy.RUNTIME)
    @Target({ElementType.TYPE, ElementType.METHOD})
    @interface Tag { String value(); int num() default 1; String[] tags() default {}; }

    @Tag(value = "demo", num = 7, tags = {"a", "b"})
    static class Annotated { @Tag("m") void marked() {} }

    static class Box<T> {
        private final T v;
        Box(T v) { this.v = v; }
        T get() { return v; }
        <R> Box<R> map(Function<? super T, ? extends R> f) { return new Box<>(f.apply(v)); }
    }

    @SafeVarargs
    static <T> List<T> listOf(T... xs) { return Arrays.asList(xs); }
    static <T extends Comparable<T>> T maxOf(Collection<T> xs) { return xs.stream().max(Comparator.naturalOrder()).orElseThrow(); }
    static <T> void swap(T[] a, int i, int j) { T t = a[i]; a[i] = a[j]; a[j] = t; }

    record Point(int x, int y) {
        Point scaled(int k) { return new Point(x * k, y * k); }
    }
    record Range(int lo, int hi) {
        Range { if (lo > hi) throw new IllegalArgumentException("lo>hi"); }
        int span() { return hi - lo; }
    }

    sealed interface Shape permits Circle, Square {}
    record Circle(double r) implements Shape {}
    record Square(double s) implements Shape {}
    static double area(Shape sh) {
        if (sh instanceof Circle c) return Math.PI * c.r() * c.r();
        if (sh instanceof Square q) return q.s() * q.s();
        return -1;
    }

    enum Color { RED, GREEN, BLUE }

    static class Holder {
        private int secret;
        public Holder() { this.secret = 7; }
        public Holder(int s) { this.secret = s; }
        public int getSecret() { return secret; }
        public static int twice(int x) { return x * 2; }
    }

    public static void main(String[] args) {
        section(StdlibTest::testLists, "lists");
        section(StdlibTest::testSets, "sets");
        section(StdlibTest::testMaps, "maps");
        section(StdlibTest::testNavigable, "navigable");
        section(StdlibTest::testDequeQueue, "deque-queue");
        section(StdlibTest::testIterators, "iterators");
        section(StdlibTest::testStreamCore, "stream-core");
        section(StdlibTest::testPrimitiveStreams, "primitive-streams");
        section(StdlibTest::testCollectors, "collectors");
        section(StdlibTest::testOptional, "optional");
        section(StdlibTest::testComparator, "comparator");
        section(StdlibTest::testFunctional, "functional");
        section(StdlibTest::testGenerics, "generics");
        section(StdlibTest::testReflection, "reflection");
        section(StdlibTest::testRecordsSealed, "records-sealed");
        section(StdlibTest::testLangFeatures, "lang-features");
        section(StdlibTest::testEnums, "enums");
        section(StdlibTest::testObjects, "objects");
        section(StdlibTest::testArrays, "arrays");
        section(StdlibTest::testCollectionsUtil, "collections-util");
        section(StdlibTest::testStringJoiner, "stringjoiner");

        System.out.println("STDLIB_RESULT ok=" + ok + " fail=" + fail);
        if (fail == 0) System.out.println("STDLIB_DONE");
    }

    // ============================ Lists ============================
    static void testLists() {
        List<Integer> al = new ArrayList<>(List.of(10, 20, 30));
        al.add(40); al.add(1, 15);
        eq(al, List.of(10, 15, 20, 30, 40), "arraylist-add-index");
        eq(al.get(2), 20, "list-get");
        eq(al.size(), 5, "list-size");
        eq(al.indexOf(30), 3, "list-indexOf");
        eq(al.lastIndexOf(40), 4, "list-lastIndexOf");
        check(al.contains(15) && !al.contains(99), "list-contains");
        al.set(0, 11);
        eq(al.get(0), 11, "list-set");
        al.remove(Integer.valueOf(15)); // remove by object
        eq(al, List.of(11, 20, 30, 40), "list-remove-object");
        al.remove(0); // remove by index
        eq(al, List.of(20, 30, 40), "list-remove-index");
        al.removeIf(x -> x == 30);
        eq(al, List.of(20, 40), "list-removeIf");
        al.replaceAll(x -> x + 1);
        eq(al, List.of(21, 41), "list-replaceAll");

        List<Integer> sub = new ArrayList<>(List.of(1, 2, 3, 4, 5)).subList(1, 4);
        eq(sub, List.of(2, 3, 4), "list-subList");

        LinkedList<Integer> ll = new LinkedList<>(List.of(2, 3));
        ll.addFirst(1); ll.addLast(4);
        eq(ll.getFirst(), 1, "linkedlist-getFirst");
        eq(ll.getLast(), 4, "linkedlist-getLast");
        eq(ll.peekFirst(), 1, "linkedlist-peekFirst");
        eq(ll, List.of(1, 2, 3, 4), "linkedlist-order");

        // retainAll / removeAll / containsAll
        List<Integer> r = new ArrayList<>(List.of(1, 2, 3, 4, 5, 6));
        r.retainAll(List.of(2, 4, 6, 8));
        eq(r, List.of(2, 4, 6), "list-retainAll");
        List<Integer> rm = new ArrayList<>(List.of(1, 2, 3, 4, 5));
        rm.removeAll(List.of(2, 4));
        eq(rm, List.of(1, 3, 5), "list-removeAll");
        check(new ArrayList<>(List.of(1, 2, 3)).containsAll(List.of(1, 3)), "list-containsAll");

        // sort + toArray + copyOf
        List<Integer> srt = new ArrayList<>(List.of(3, 1, 2));
        srt.sort(Comparator.naturalOrder());
        eq(srt, List.of(1, 2, 3), "list-sort");
        srt.sort(Comparator.reverseOrder());
        eq(srt, List.of(3, 2, 1), "list-sort-reverse");
        Integer[] arr = new ArrayList<>(List.of(7, 8)).toArray(new Integer[0]);
        eq(arr.length, 2, "list-toArray");
        eq(List.copyOf(List.of(1, 2, 3)), List.of(1, 2, 3), "list-copyOf");

        // Stack (legacy)
        Stack<Integer> st = new Stack<>();
        st.push(1); st.push(2); st.push(3);
        eq(st.pop(), 3, "stack-pop");
        eq(st.peek(), 2, "stack-peek");
        eq(st.search(1), 2, "stack-search");

        // immutability of factory/view collections
        throwsEx(UnsupportedOperationException.class, () -> List.of(1, 2).add(3), "list-of-immutable");
        throwsEx(UnsupportedOperationException.class, () -> Arrays.asList(1, 2, 3).add(4), "aslist-fixedsize");
        throwsEx(UnsupportedOperationException.class, () -> Collections.unmodifiableList(new ArrayList<>(List.of(1))).add(2), "unmodifiable-add");
        throwsEx(UnsupportedOperationException.class, () -> Stream.of(1, 2).toList().add(3), "stream-toList-immutable");
        throwsEx(IndexOutOfBoundsException.class, () -> List.of(1).get(9), "list-index-oob");

        // Arrays.asList allows set but not size change
        List<Integer> fixed = Arrays.asList(1, 2, 3);
        fixed.set(0, 9);
        eq(fixed.get(0), 9, "aslist-set-ok");
    }

    // ============================ Sets ============================
    static void testSets() {
        Set<Integer> hs = new HashSet<>(List.of(1, 2, 3, 3, 2));
        eq(hs.size(), 3, "hashset-dedup");
        check(hs.add(4) && !hs.add(4), "hashset-add-return");
        check(hs.remove(4) && !hs.remove(99), "hashset-remove-return");

        LinkedHashSet<String> lhs = new LinkedHashSet<>();
        lhs.add("c"); lhs.add("a"); lhs.add("b"); lhs.add("a");
        eq(new ArrayList<>(lhs), List.of("c", "a", "b"), "linkedhashset-order");

        // set algebra
        Set<Integer> a = new HashSet<>(List.of(1, 2, 3, 4));
        Set<Integer> b = new HashSet<>(List.of(3, 4, 5, 6));
        Set<Integer> union = new TreeSet<>(a); union.addAll(b);
        eq(new ArrayList<>(union), List.of(1, 2, 3, 4, 5, 6), "set-union");
        Set<Integer> inter = new TreeSet<>(a); inter.retainAll(b);
        eq(new ArrayList<>(inter), List.of(3, 4), "set-intersection");
        Set<Integer> diff = new TreeSet<>(a); diff.removeAll(b);
        eq(new ArrayList<>(diff), List.of(1, 2), "set-difference");
        check(a.containsAll(List.of(1, 2)), "set-containsAll");

        throwsEx(UnsupportedOperationException.class, () -> Set.of(1, 2).add(3), "set-of-immutable");
        throwsEx(IllegalArgumentException.class, () -> Set.of(1, 1), "set-of-dup-throws");

        // EnumSet
        EnumSet<Color> es = EnumSet.of(Color.RED, Color.BLUE);
        check(es.contains(Color.RED) && !es.contains(Color.GREEN), "enumset-of");
        eq(EnumSet.allOf(Color.class).size(), 3, "enumset-allOf");
        eq(EnumSet.range(Color.RED, Color.GREEN).size(), 2, "enumset-range");
        eq(EnumSet.complementOf(EnumSet.of(Color.RED)), EnumSet.of(Color.GREEN, Color.BLUE), "enumset-complement");
        eq(EnumSet.noneOf(Color.class).size(), 0, "enumset-none");
    }

    // ============================ Maps ============================
    static void testMaps() {
        Map<String, Integer> m = new HashMap<>();
        m.put("a", 1); m.put("b", 2);
        eq(m.getOrDefault("a", 0), 1, "map-getOrDefault-present");
        eq(m.getOrDefault("z", 99), 99, "map-getOrDefault-absent");
        eq(m.putIfAbsent("a", 100), 1, "map-putIfAbsent-existing");
        eq(m.putIfAbsent("c", 3), null, "map-putIfAbsent-new");
        eq(m.get("c"), 3, "map-putIfAbsent-stored");
        m.merge("a", 10, Integer::sum);
        eq(m.get("a"), 11, "map-merge-sum");
        m.merge("d", 5, Integer::sum);
        eq(m.get("d"), 5, "map-merge-absent");
        m.compute("b", (k, v) -> v + 100);
        eq(m.get("b"), 102, "map-compute");
        m.computeIfAbsent("e", k -> 42);
        eq(m.get("e"), 42, "map-computeIfAbsent");
        m.computeIfPresent("e", (k, v) -> v + 1);
        eq(m.get("e"), 43, "map-computeIfPresent");
        check(m.replace("e", 43, 44), "map-replace-conditional");
        eq(m.get("e"), 44, "map-replace-stored");
        check(m.remove("e", 44), "map-remove-conditional");
        check(!m.containsKey("e"), "map-removed");

        Map<String, Integer> rm = new HashMap<>(Map.of("x", 1, "y", 2, "z", 3));
        rm.replaceAll((k, v) -> v * 10);
        eq(new TreeMap<>(rm), new TreeMap<>(Map.of("x", 10, "y", 20, "z", 30)), "map-replaceAll");

        // LinkedHashMap insertion-order
        LinkedHashMap<String, Integer> lhm = new LinkedHashMap<>();
        lhm.put("one", 1); lhm.put("two", 2); lhm.put("three", 3);
        eq(new ArrayList<>(lhm.keySet()), List.of("one", "two", "three"), "linkedhashmap-order");

        // LinkedHashMap access-order
        LinkedHashMap<String, Integer> acc = new LinkedHashMap<>(16, 0.75f, true);
        acc.put("a", 1); acc.put("b", 2); acc.put("c", 3);
        acc.get("a"); // moves "a" to the end
        eq(new ArrayList<>(acc.keySet()), List.of("b", "c", "a"), "linkedhashmap-access-order");

        // Map.of / Map.entry / Map.ofEntries
        eq(Map.of("k", 5).get("k"), 5, "map-of");
        Map.Entry<String, Integer> ent = Map.entry("p", 7);
        eq(ent.getKey(), "p", "map-entry-key");
        eq(ent.getValue(), 7, "map-entry-value");
        Map<String, Integer> me = Map.ofEntries(Map.entry("a", 1), Map.entry("b", 2));
        eq(me.get("b"), 2, "map-ofEntries");
        throwsEx(UnsupportedOperationException.class, () -> Map.of("a", 1).put("b", 2), "map-of-immutable");

        // entrySet sum + forEach
        int[] tot = {0};
        Map.of("a", 1, "b", 2, "c", 3).forEach((k, v) -> tot[0] += v);
        eq(tot[0], 6, "map-forEach-sum");
        int esum = Map.of("a", 1, "b", 2, "c", 3).entrySet().stream().mapToInt(Map.Entry::getValue).sum();
        eq(esum, 6, "map-entryset-stream-sum");

        // EnumMap
        EnumMap<Color, String> em = new EnumMap<>(Color.class);
        em.put(Color.GREEN, "g"); em.put(Color.RED, "r");
        eq(new ArrayList<>(em.keySet()), List.of(Color.RED, Color.GREEN), "enummap-natural-order");
        eq(em.get(Color.RED), "r", "enummap-get");

        // values multiset
        eq(new TreeMap<>(Map.of("a", 1, "b", 2)).values().stream().mapToInt(Integer::intValue).sum(), 3, "map-values-sum");
    }

    // ============================ Navigable (TreeMap / TreeSet) ============================
    static void testNavigable() {
        TreeMap<Integer, String> tm = new TreeMap<>();
        for (int k : new int[]{1, 2, 3, 4, 5}) tm.put(k, "v" + k);
        eq(tm.firstKey(), 1, "treemap-firstKey");
        eq(tm.lastKey(), 5, "treemap-lastKey");
        eq(tm.floorKey(3), 3, "treemap-floorKey-exact");
        eq(tm.floorKey(0), null, "treemap-floorKey-none");
        eq(tm.ceilingKey(3), 3, "treemap-ceilingKey");
        eq(tm.higherKey(3), 4, "treemap-higherKey");
        eq(tm.lowerKey(3), 2, "treemap-lowerKey");
        eq(new ArrayList<>(tm.headMap(3).keySet()), List.of(1, 2), "treemap-headMap");
        eq(new ArrayList<>(tm.headMap(3, true).keySet()), List.of(1, 2, 3), "treemap-headMap-incl");
        eq(new ArrayList<>(tm.tailMap(3).keySet()), List.of(3, 4, 5), "treemap-tailMap");
        eq(new ArrayList<>(tm.subMap(2, 4).keySet()), List.of(2, 3), "treemap-subMap");
        eq(new ArrayList<>(tm.subMap(2, true, 4, true).keySet()), List.of(2, 3, 4), "treemap-subMap-incl");
        eq(tm.firstEntry().getKey(), 1, "treemap-firstEntry");
        eq(new ArrayList<>(tm.descendingMap().keySet()), List.of(5, 4, 3, 2, 1), "treemap-descendingMap");
        TreeMap<Integer, String> poll = new TreeMap<>(tm);
        eq(poll.pollFirstEntry().getKey(), 1, "treemap-pollFirstEntry");
        eq(poll.firstKey(), 2, "treemap-pollFirst-effect");

        TreeSet<Integer> ts = new TreeSet<>(List.of(5, 1, 3, 2, 4));
        eq(new ArrayList<>(ts), List.of(1, 2, 3, 4, 5), "treeset-sorted");
        eq(ts.first(), 1, "treeset-first");
        eq(ts.last(), 5, "treeset-last");
        eq(ts.floor(3), 3, "treeset-floor");
        eq(ts.ceiling(3), 3, "treeset-ceiling");
        eq(ts.higher(3), 4, "treeset-higher");
        eq(ts.lower(3), 2, "treeset-lower");
        eq(new ArrayList<>(ts.headSet(3)), List.of(1, 2), "treeset-headSet");
        eq(new ArrayList<>(ts.tailSet(3)), List.of(3, 4, 5), "treeset-tailSet");
        eq(new ArrayList<>(ts.subSet(2, 5)), List.of(2, 3, 4), "treeset-subSet");
        eq(new ArrayList<>(ts.descendingSet()), List.of(5, 4, 3, 2, 1), "treeset-descendingSet");
        TreeSet<Integer> pts = new TreeSet<>(ts);
        eq(pts.pollFirst(), 1, "treeset-pollFirst");
        eq(pts.pollLast(), 5, "treeset-pollLast");

        // custom comparator TreeSet
        TreeSet<String> byLen = new TreeSet<>(Comparator.comparingInt(String::length).thenComparing(Comparator.naturalOrder()));
        byLen.addAll(List.of("ccc", "a", "bb", "b", "dd"));
        eq(new ArrayList<>(byLen), List.of("a", "b", "bb", "dd", "ccc"), "treeset-custom-comparator");
    }

    // ============================ Deque / Queue ============================
    static void testDequeQueue() {
        ArrayDeque<Integer> dq = new ArrayDeque<>();
        dq.offerLast(2); dq.offerLast(3); dq.offerFirst(1);
        eq(dq.peekFirst(), 1, "arraydeque-peekFirst");
        eq(dq.peekLast(), 3, "arraydeque-peekLast");
        eq(dq.pollFirst(), 1, "arraydeque-pollFirst");
        eq(dq.pollLast(), 3, "arraydeque-pollLast");
        eq(dq.peekFirst(), 2, "arraydeque-remaining");

        // ArrayDeque as a stack
        ArrayDeque<Integer> st = new ArrayDeque<>();
        st.push(1); st.push(2); st.push(3);
        eq(st.pop(), 3, "arraydeque-stack-pop");
        eq(st.peek(), 2, "arraydeque-stack-peek");
        eq(new ArrayList<>(st), List.of(2, 1), "arraydeque-stack-order");

        // descendingIterator
        ArrayDeque<Integer> d2 = new ArrayDeque<>(List.of(1, 2, 3));
        List<Integer> rev = new ArrayList<>();
        d2.descendingIterator().forEachRemaining(rev::add);
        eq(rev, List.of(3, 2, 1), "arraydeque-descendingIterator");

        // PriorityQueue natural order
        PriorityQueue<Integer> pq = new PriorityQueue<>();
        pq.addAll(List.of(5, 1, 3, 2, 4));
        List<Integer> drained = new ArrayList<>();
        while (!pq.isEmpty()) drained.add(pq.poll());
        eq(drained, List.of(1, 2, 3, 4, 5), "priorityqueue-natural");

        // PriorityQueue with comparator (max-heap)
        PriorityQueue<Integer> maxpq = new PriorityQueue<>(Comparator.reverseOrder());
        maxpq.addAll(List.of(5, 1, 3, 2, 4));
        eq(maxpq.poll(), 5, "priorityqueue-max-poll");

        // LinkedList as queue (FIFO)
        Queue<Integer> q = new LinkedList<>();
        q.offer(1); q.offer(2); q.offer(3);
        eq(q.poll(), 1, "linkedlist-queue-fifo");
        eq(q.peek(), 2, "linkedlist-queue-peek");

        // empty-deque exception
        throwsEx(NoSuchElementException.class, () -> new ArrayDeque<Integer>().getFirst(), "arraydeque-empty-getFirst");
    }

    // ============================ Iterators ============================
    static void testIterators() {
        List<Integer> l = new ArrayList<>(List.of(1, 2, 3, 4, 5, 6));
        Iterator<Integer> it = l.iterator();
        int oddSum = 0;
        while (it.hasNext()) {
            int v = it.next();
            if (v % 2 == 0) it.remove();
            else oddSum += v;
        }
        eq(l, List.of(1, 3, 5), "iterator-remove");
        eq(oddSum, 9, "iterator-remove-sum");

        ListIterator<Integer> lit = l.listIterator();
        lit.next();
        lit.set(99);
        eq(l.get(0), 99, "listiterator-set");
        // walk to the end then go backwards
        while (lit.hasNext()) lit.next();
        List<Integer> back = new ArrayList<>();
        while (lit.hasPrevious()) back.add(lit.previous());
        eq(back, List.of(5, 3, 99), "listiterator-previous");

        // ListIterator.add
        List<Integer> la = new ArrayList<>(List.of(1, 3));
        ListIterator<Integer> insIt = la.listIterator();
        insIt.next();
        insIt.add(2);
        eq(la, List.of(1, 2, 3), "listiterator-add");

        // fail-fast ConcurrentModificationException
        throwsEx(ConcurrentModificationException.class, () -> {
            List<Integer> c = new ArrayList<>(List.of(1, 2, 3));
            for (Integer x : c) c.add(x);
        }, "iterator-cme");

        // exhausted iterator
        throwsEx(NoSuchElementException.class, () -> {
            Iterator<Integer> e = List.of(1).iterator();
            e.next(); e.next();
        }, "iterator-exhausted");
    }

    // ============================ Stream core ============================
    static void testStreamCore() {
        eq(Stream.of(1, 2, 3, 4, 5).map(x -> x * x).collect(Collectors.toList()), List.of(1, 4, 9, 16, 25), "stream-map");
        eq(Stream.of(1, 2, 3, 4, 5, 6).filter(x -> x % 2 == 0).toList(), List.of(2, 4, 6), "stream-filter");
        eq(Stream.of("x", "y", "z").reduce("", String::concat), "xyz", "stream-reduce-identity");
        eq(Stream.of(1, 2, 3, 4).reduce(Integer::sum).orElseThrow(), 10, "stream-reduce-noid");
        eq(Stream.of(1, 1, 2, 2, 3, 3).distinct().toList(), List.of(1, 2, 3), "stream-distinct");
        eq(Stream.of(3, 1, 2).sorted().toList(), List.of(1, 2, 3), "stream-sorted");
        eq(Stream.of(1, 2, 3).sorted(Comparator.reverseOrder()).toList(), List.of(3, 2, 1), "stream-sorted-comp");
        eq(Stream.iterate(1, x -> x + 1).skip(2).limit(3).toList(), List.of(3, 4, 5), "stream-skip-limit");
        eq(Stream.of(List.of(1, 2), List.of(3, 4)).flatMap(List::stream).toList(), List.of(1, 2, 3, 4), "stream-flatMap");
        eq(Stream.of(1, 2, 3).count(), 3L, "stream-count");
        check(Stream.of(2, 4, 6).allMatch(x -> x % 2 == 0), "stream-allMatch");
        check(Stream.of(1, 2, 3).anyMatch(x -> x == 2), "stream-anyMatch");
        check(Stream.of(1, 3, 5).noneMatch(x -> x % 2 == 0), "stream-noneMatch");
        eq(Stream.of(5, 3, 8, 1).max(Comparator.naturalOrder()).orElseThrow(), 8, "stream-max");
        eq(Stream.of(5, 3, 8, 1).min(Comparator.naturalOrder()).orElseThrow(), 1, "stream-min");
        eq(Stream.of(7, 8, 9).findFirst().orElseThrow(), 7, "stream-findFirst");
        check(Stream.of(1, 2, 3).findAny().isPresent(), "stream-findAny");

        // peek side effect
        int[] seen = {0};
        long c = Stream.of(1, 2, 3).peek(x -> seen[0] += x).count();
        eq(c, 3L, "stream-peek-count");

        // mapToInt / boxed / toArray
        eq(Stream.of("a", "bb", "ccc").mapToInt(String::length).sum(), 6, "stream-mapToInt-sum");
        Integer[] ia = Stream.of(1, 2, 3).toArray(Integer[]::new);
        eq(ia.length, 3, "stream-toArray");

        // concat / generate / iterate(3-arg) / ofNullable / takeWhile / dropWhile
        eq(Stream.concat(Stream.of(1, 2), Stream.of(3, 4)).toList(), List.of(1, 2, 3, 4), "stream-concat");
        eq(Stream.generate(() -> 7).limit(3).distinct().count(), 1L, "stream-generate");
        eq(Stream.iterate(1, x -> x < 100, x -> x * 2).toList(), List.of(1, 2, 4, 8, 16, 32, 64), "stream-iterate-3arg");
        eq(Stream.ofNullable(null).count(), 0L, "stream-ofNullable-null");
        eq(Stream.ofNullable("x").count(), 1L, "stream-ofNullable-value");
        eq(Stream.of(1, 2, 3, 4, 1, 2).takeWhile(x -> x < 3).toList(), List.of(1, 2), "stream-takeWhile");
        eq(Stream.of(1, 2, 3, 4, 1, 2).dropWhile(x -> x < 3).toList(), List.of(3, 4, 1, 2), "stream-dropWhile");

        // empty stream reductions
        eq(Stream.<Integer>of().findFirst(), Optional.empty(), "stream-empty-findFirst");
        eq(Stream.<Integer>of().reduce(Integer::sum), Optional.empty(), "stream-empty-reduce");
    }

    // ============================ Primitive streams ============================
    static void testPrimitiveStreams() {
        eq(IntStream.range(0, 5).sum(), 10, "intstream-range-sum");
        eq(IntStream.rangeClosed(1, 5).sum(), 15, "intstream-rangeClosed-sum");
        eq(IntStream.rangeClosed(1, 4).reduce(1, (a, b) -> a * b), 24, "intstream-reduce-product");
        eq(IntStream.range(0, 10).filter(i -> i % 2 == 0).count(), 5L, "intstream-filter-count");
        eq(IntStream.range(0, 5).mapToObj(Integer::toString).collect(Collectors.joining()), "01234", "intstream-mapToObj");
        eq(IntStream.range(0, 3).boxed().toList(), List.of(0, 1, 2), "intstream-boxed");
        eq(IntStream.of(3, 1, 2).max().getAsInt(), 3, "intstream-max");
        eq(IntStream.of(3, 1, 2).min().getAsInt(), 1, "intstream-min");
        closeEq(IntStream.of(1, 2, 3, 4).average().getAsDouble(), 2.5, "intstream-average");

        IntSummaryStatistics ss = IntStream.of(1, 2, 3, 4).summaryStatistics();
        eq(ss.getMax(), 4, "intstats-max");
        eq(ss.getMin(), 1, "intstats-min");
        eq(ss.getSum(), 10L, "intstats-sum");
        eq(ss.getCount(), 4L, "intstats-count");
        closeEq(ss.getAverage(), 2.5, "intstats-average");

        eq(IntStream.range(0, 5).asLongStream().sum(), 10L, "intstream-asLong");
        closeEq(IntStream.of(1, 2).asDoubleStream().sum(), 3.0, "intstream-asDouble");
        eq(LongStream.rangeClosed(1, 5).sum(), 15L, "longstream-sum");
        closeEq(DoubleStream.of(1.5, 2.5, 3.0).sum(), 7.0, "doublestream-sum");
        eq(IntStream.of(1, 2, 3).mapToLong(i -> (long) i * i).sum(), 14L, "intstream-mapToLong");
        eq(IntStream.iterate(1, i -> i <= 16, i -> i * 2).count(), 5L, "intstream-iterate-3arg");
    }

    // ============================ Collectors ============================
    static void testCollectors() {
        eq(Stream.of(1, 2, 3).collect(Collectors.toList()), List.of(1, 2, 3), "collect-toList");
        eq(Stream.of(1, 2, 2, 3).collect(Collectors.toSet()), new HashSet<>(List.of(1, 2, 3)), "collect-toSet");
        eq(Stream.of(1, 2, 3).collect(Collectors.counting()), 3L, "collect-counting");
        eq(Stream.of(1, 2, 3, 4).collect(Collectors.summingInt(i -> i)), 10, "collect-summingInt");
        closeEq(Stream.of(1, 2, 3, 4).collect(Collectors.averagingInt(i -> i)), 2.5, "collect-averagingInt");
        eq(Stream.of(3, 1, 2).collect(Collectors.maxBy(Comparator.naturalOrder())).orElseThrow(), 3, "collect-maxBy");
        eq(Stream.of(3, 1, 2).collect(Collectors.minBy(Comparator.naturalOrder())).orElseThrow(), 1, "collect-minBy");
        eq(Stream.of(1, 2, 3, 4).collect(Collectors.reducing(0, Integer::sum)), 10, "collect-reducing");

        // joining variants
        eq(Stream.of("a", "b", "c").collect(Collectors.joining()), "abc", "collect-joining");
        eq(Stream.of("a", "b", "c").collect(Collectors.joining("-")), "a-b-c", "collect-joining-sep");
        eq(Stream.of("a", "b", "c").collect(Collectors.joining(",", "[", "]")), "[a,b,c]", "collect-joining-fix");

        // toMap (+ merge function)
        eq(Stream.of("a", "bb", "ccc").collect(Collectors.toMap(s -> s, String::length)).get("ccc"), 3, "collect-toMap");
        Map<Character, Integer> firstCharCount = Stream.of("apple", "ant", "bee")
                .collect(Collectors.toMap(s -> s.charAt(0), s -> 1, Integer::sum));
        eq(firstCharCount.get('a'), 2, "collect-toMap-merge");

        // groupingBy + downstreams
        Map<Integer, List<String>> byLen = Stream.of("a", "bb", "cc", "ddd")
                .collect(Collectors.groupingBy(String::length));
        eq(byLen.get(2), List.of("bb", "cc"), "collect-groupingBy");
        Map<Character, Long> counts = Stream.of("apple", "avocado", "banana", "cherry", "blueberry")
                .collect(Collectors.groupingBy(s -> s.charAt(0), Collectors.counting()));
        eq(counts.get('a'), 2L, "collect-groupingBy-counting");
        Map<Integer, List<Character>> firstChars = Stream.of("ab", "cd", "ef", "ghi")
                .collect(Collectors.groupingBy(String::length, Collectors.mapping(s -> s.charAt(0), Collectors.toList())));
        eq(firstChars.get(2), List.of('a', 'c', 'e'), "collect-groupingBy-mapping");
        Map<Boolean, Integer> sumByParity = Stream.of(1, 2, 3, 4, 5, 6)
                .collect(Collectors.groupingBy(i -> i % 2 == 0, Collectors.summingInt(i -> i)));
        eq(sumByParity.get(true), 12, "collect-groupingBy-summing");
        Map<Integer, List<Integer>> filteredGroups = Stream.of(1, 2, 3, 4, 5, 6)
                .collect(Collectors.groupingBy(i -> i % 2, Collectors.filtering(i -> i > 2, Collectors.toList())));
        eq(filteredGroups.get(0), List.of(4, 6), "collect-groupingBy-filtering");
        Map<Integer, TreeSet<Integer>> tsGroups = Stream.of(3, 1, 4, 1, 5, 9, 2, 6)
                .collect(Collectors.groupingBy(i -> i % 2, Collectors.toCollection(TreeSet::new)));
        eq(new ArrayList<>(tsGroups.get(1)), List.of(1, 3, 5, 9), "collect-groupingBy-toCollection");

        // partitioningBy + downstream
        Map<Boolean, List<Integer>> parts = Stream.of(1, 2, 3, 4, 5, 6)
                .collect(Collectors.partitioningBy(i -> i % 2 == 0));
        eq(parts.get(true), List.of(2, 4, 6), "collect-partitioningBy");
        Map<Boolean, Long> partCounts = Stream.of(1, 2, 3, 4, 5)
                .collect(Collectors.partitioningBy(i -> i > 2, Collectors.counting()));
        eq(partCounts.get(true), 3L, "collect-partitioningBy-counting");

        // summarizing
        IntSummaryStatistics st = Stream.of(1, 2, 3, 4).collect(Collectors.summarizingInt(i -> i));
        eq(st.getMax(), 4, "collect-summarizingInt");

        // collectingAndThen, teeing, toUnmodifiableList, flatMapping
        eq(Stream.of(1, 2, 3).collect(Collectors.collectingAndThen(Collectors.toList(), List::size)), 3, "collect-collectingAndThen");
        double avg = Stream.of(1, 2, 3, 4).collect(Collectors.teeing(
                Collectors.summingInt(i -> i), Collectors.counting(), (sum, cnt) -> sum / (double) cnt));
        closeEq(avg, 2.5, "collect-teeing");
        eq(Stream.of(List.of(1, 2), List.of(3, 4)).collect(Collectors.flatMapping(List::stream, Collectors.toList())),
                List.of(1, 2, 3, 4), "collect-flatMapping");
        throwsEx(UnsupportedOperationException.class,
                () -> Stream.of(1, 2).collect(Collectors.toUnmodifiableList()).add(3), "collect-toUnmodifiableList");
    }

    // ============================ Optional ============================
    static void testOptional() {
        eq(Optional.of(5).map(x -> x * 2).get(), 10, "optional-map");
        eq(Optional.of(5).filter(x -> x > 10).isPresent(), false, "optional-filter-out");
        eq(Optional.of(5).flatMap(x -> Optional.of(x + 1)).get(), 6, "optional-flatMap");
        eq(Optional.empty().orElse("def"), "def", "optional-orElse");
        eq(Optional.empty().orElseGet(() -> "lazy"), "lazy", "optional-orElseGet");
        check(Optional.ofNullable(null).isEmpty(), "optional-ofNullable-null");
        check(Optional.ofNullable("x").isPresent(), "optional-ofNullable-value");
        eq(Optional.empty().or(() -> Optional.of(9)).get(), 9, "optional-or");
        int[] sink = {0};
        Optional.of(3).ifPresent(v -> sink[0] = v);
        eq(sink[0], 3, "optional-ifPresent");
        Optional.empty().ifPresentOrElse(v -> {}, () -> sink[0] = -1);
        eq(sink[0], -1, "optional-ifPresentOrElse");
        throwsEx(NoSuchElementException.class, () -> Optional.empty().get(), "optional-empty-get");
        throwsEx(IllegalStateException.class, () -> Optional.empty().orElseThrow(IllegalStateException::new), "optional-orElseThrow");

        // primitive optionals
        eq(OptionalInt.of(5).getAsInt(), 5, "optionalint-get");
        eq(OptionalInt.empty().orElse(7), 7, "optionalint-orElse");
        closeEq(OptionalDouble.of(1.5).getAsDouble(), 1.5, "optionaldouble-get");
        eq(OptionalLong.of(9L).getAsLong(), 9L, "optionallong-get");
        eq(IntStream.range(0, 5).max(), OptionalInt.of(4), "intstream-max-optional");
    }

    // ============================ Comparator ============================
    static void testComparator() {
        List<String> words = new ArrayList<>(List.of("bb", "a", "cc", "aaa", "b"));
        words.sort(Comparator.comparingInt(String::length).thenComparing(Comparator.naturalOrder()));
        eq(words, List.of("a", "b", "bb", "cc", "aaa"), "comparator-comparing-thenComparing");

        List<Integer> nums = new ArrayList<>(List.of(3, 1, 2));
        nums.sort(Comparator.<Integer>naturalOrder().reversed());
        eq(nums, List.of(3, 2, 1), "comparator-reversed");

        eq(Collections.max(List.of(3, 7, 2), Comparator.naturalOrder()), 7, "comparator-max");
        eq(Collections.min(List.of("ccc", "a", "bb"), Comparator.comparingInt(String::length)), "a", "comparator-min-by-len");

        // nullsFirst / nullsLast
        List<String> withNull = new ArrayList<>(Arrays.asList("b", null, "a"));
        withNull.sort(Comparator.nullsFirst(Comparator.naturalOrder()));
        eq(withNull, Arrays.asList(null, "a", "b"), "comparator-nullsFirst");
        List<String> withNull2 = new ArrayList<>(Arrays.asList("b", null, "a"));
        withNull2.sort(Comparator.nullsLast(Comparator.naturalOrder()));
        eq(withNull2, Arrays.asList("a", "b", null), "comparator-nullsLast");

        // comparingDouble + reverse comparison sign
        eq(Comparator.<Integer>reverseOrder().compare(1, 2) > 0, true, "comparator-reverseOrder-sign");
        List<double[]> pts = new ArrayList<>(List.of(new double[]{3.0}, new double[]{1.0}, new double[]{2.0}));
        pts.sort(Comparator.comparingDouble(p -> p[0]));
        closeEq(pts.get(0)[0], 1.0, "comparator-comparingDouble");
    }

    // ============================ Functional interfaces ============================
    static void testFunctional() {
        Function<Integer, Integer> inc = x -> x + 1;
        Function<Integer, Integer> dbl = x -> x * 2;
        eq(inc.andThen(dbl).apply(3), 8, "function-andThen");
        eq(inc.compose(dbl).apply(3), 7, "function-compose");
        eq(Function.<Integer>identity().apply(42), 42, "function-identity");

        BiFunction<Integer, Integer, Integer> add = (a, b) -> a + b;
        eq(add.andThen(x -> x * 10).apply(2, 3), 50, "bifunction-andThen");

        Predicate<Integer> even = x -> x % 2 == 0;
        Predicate<Integer> pos = x -> x > 0;
        check(even.and(pos).test(4), "predicate-and");
        check(even.or(pos).test(3), "predicate-or");
        check(even.negate().test(3), "predicate-negate");
        check(Predicate.isEqual("hi").test("hi"), "predicate-isEqual");
        check(Predicate.not(String::isBlank).test("x"), "predicate-not");

        Supplier<String> sup = () -> "supplied";
        eq(sup.get(), "supplied", "supplier-get");

        StringBuilder cb = new StringBuilder();
        Consumer<String> c1 = cb::append;
        Consumer<String> c2 = s -> cb.append(s.toUpperCase(Locale.ROOT));
        c1.andThen(c2).accept("ab");
        eq(cb.toString(), "abAB", "consumer-andThen");

        UnaryOperator<String> up = s -> s + "!";
        eq(up.apply("go"), "go!", "unaryoperator");
        BinaryOperator<Integer> minOp = BinaryOperator.minBy(Comparator.naturalOrder());
        eq(minOp.apply(3, 5), 3, "binaryoperator-minBy");
        BinaryOperator<Integer> maxOp = BinaryOperator.maxBy(Comparator.naturalOrder());
        eq(maxOp.apply(3, 5), 5, "binaryoperator-maxBy");

        // primitive specializations
        IntUnaryOperator sq = x -> x * x;
        eq(sq.applyAsInt(6), 36, "intunaryoperator");
        IntBinaryOperator mul = (a, b) -> a * b;
        eq(mul.applyAsInt(4, 5), 20, "intbinaryoperator");
        ToIntFunction<String> len = String::length;
        eq(len.applyAsInt("hello"), 5, "tointfunction");
        IntPredicate isOdd = x -> x % 2 == 1;
        check(isOdd.test(7), "intpredicate");
        IntFunction<String> toStr = Integer::toString;
        eq(toStr.apply(99), "99", "intfunction");
        Supplier<List<Integer>> factory = ArrayList::new;
        eq(factory.get().size(), 0, "supplier-constructor-ref");
    }

    // ============================ Generics ============================
    static void testGenerics() {
        eq(maxOf(List.of(3, 7, 2, 5)), 7, "generics-bounded-max");
        eq(maxOf(List.of("apple", "banana", "cherry")), "cherry", "generics-bounded-max-string");

        Box<Integer> b = new Box<>(10);
        Box<String> mapped = b.map(x -> "n=" + x);
        eq(mapped.get(), "n=10", "generics-box-map");

        eq(StdlibTest.<String>listOf("p", "q", "r"), List.of("p", "q", "r"), "generics-varargs");

        Integer[] sw = {1, 2, 3};
        swap(sw, 0, 2);
        eq(Arrays.asList(sw), List.of(3, 2, 1), "generics-swap");

        // wildcard: sum of a Collection<? extends Number>
        Collection<? extends Number> nums = List.of(1, 2.5, 3L);
        double s = 0;
        for (Number n : nums) s += n.doubleValue();
        closeEq(s, 6.5, "generics-wildcard-sum");
    }

    // ============================ Annotations + Reflection ============================
    static void testReflection() throws Exception {
        Tag t = Annotated.class.getAnnotation(Tag.class);
        check(t != null, "annotation-present");
        eq(t.value(), "demo", "annotation-value");
        eq(t.num(), 7, "annotation-explicit-member");
        eq(Arrays.asList(t.tags()), List.of("a", "b"), "annotation-array-member");
        check(Annotated.class.isAnnotationPresent(Tag.class), "annotation-isPresent");
        // default member value
        Method marked = Annotated.class.getDeclaredMethod("marked");
        eq(marked.getAnnotation(Tag.class).num(), 1, "annotation-default-member");

        // Class metadata
        eq(Point.class.getSimpleName(), "Point", "class-simpleName");
        check(Point.class.getName().endsWith("StdlibTest$Point"), "class-name");
        check(Color.class.isEnum(), "class-isEnum");
        check(Shape.class.isInterface(), "class-isInterface");
        check(int[].class.isArray(), "class-isArray");
        check(Point.class.isRecord(), "class-isRecord");
        check(!Holder.class.isRecord(), "class-not-record");
        check(Number.class.isAssignableFrom(Integer.class), "class-isAssignableFrom");
        eq(Integer.class.getSuperclass(), Number.class, "class-getSuperclass");

        // record components
        RecordComponent[] rcs = Point.class.getRecordComponents();
        eq(rcs.length, 2, "record-components-count");
        eq(rcs[0].getName(), "x", "record-component-name");

        // Method.invoke (static + instance)
        Method twice = Holder.class.getMethod("twice", int.class);
        eq(twice.invoke(null, 21), 42, "reflect-invoke-static");
        Method lenM = String.class.getMethod("length");
        eq(lenM.invoke("abcd"), 4, "reflect-invoke-instance");

        // Constructor.newInstance
        Constructor<Holder> ctor = Holder.class.getDeclaredConstructor(int.class);
        Holder h = ctor.newInstance(15);
        eq(h.getSecret(), 15, "reflect-newInstance");

        // Field get/set with setAccessible
        Field f = Holder.class.getDeclaredField("secret");
        f.setAccessible(true);
        Holder h2 = new Holder();
        eq(f.get(h2), 7, "reflect-field-get");
        f.set(h2, 123);
        eq(h2.getSecret(), 123, "reflect-field-set");
        check(Modifier.isPrivate(f.getModifiers()), "reflect-modifier-private");
        check(Modifier.isStatic(twice.getModifiers()), "reflect-modifier-static");

        // java.lang.reflect.Array
        Object intArr = java.lang.reflect.Array.newInstance(int.class, 3);
        java.lang.reflect.Array.setInt(intArr, 1, 55);
        eq(java.lang.reflect.Array.getInt(intArr, 1), 55, "reflect-array-set-get");
        eq(java.lang.reflect.Array.getLength(intArr), 3, "reflect-array-length");

        // enum constants
        Color[] ecs = Color.class.getEnumConstants();
        eq(ecs.length, 3, "reflect-enumConstants");

        // InvocationTargetException wraps thrown exceptions
        Method spanM = Range.class.getDeclaredMethod("span");
        throwsEx(InvocationTargetException.class, () -> {
            Constructor<Range> rc = Range.class.getDeclaredConstructor(int.class, int.class);
            rc.newInstance(5, 1); // compact ctor throws -> wrapped
        }, "reflect-InvocationTargetException");
        eq(spanM.invoke(new Range(2, 9)), 7, "reflect-invoke-record-method");
    }

    // ============================ Records + sealed + pattern instanceof ============================
    static void testRecordsSealed() {
        Point p = new Point(2, 3);
        eq(p.x(), 2, "record-accessor-x");
        eq(p.y(), 3, "record-accessor-y");
        eq(p, new Point(2, 3), "record-equals");
        eq(p.hashCode(), new Point(2, 3).hashCode(), "record-hashCode");
        check(!p.equals(new Point(2, 4)), "record-not-equals");
        eq(p.toString(), "Point[x=2, y=3]", "record-toString");
        eq(p.scaled(10), new Point(20, 30), "record-method");

        // compact constructor validation
        eq(new Range(1, 5).span(), 4, "record-compact-ok");
        throwsEx(IllegalArgumentException.class, () -> new Range(5, 1), "record-compact-throws");

        // sealed hierarchy + pattern instanceof dispatch
        closeEq(area(new Square(4)), 16.0, "sealed-square-area");
        closeEq(area(new Circle(1.0)), Math.PI, "sealed-circle-area");
        Shape sh = new Square(3);
        check(sh instanceof Square q && q.s() == 3.0, "pattern-instanceof-bind");
        check(Shape.class.isSealed(), "sealed-isSealed");
        eq(Shape.class.getPermittedSubclasses().length, 2, "sealed-permitted-count");
    }

    // ============================ Modern language stdlib features ============================
    static void testLangFeatures() {
        // switch expression: arrow, multi-label, default, yield
        for (int day = 1; day <= 7; day++) {
            String kind = switch (day) {
                case 6, 7 -> "weekend";
                default -> "weekday";
            };
            check(kind.equals(day >= 6 ? "weekend" : "weekday"), "switch-expr-arrow-" + day);
        }
        int code = 2;
        String label = switch (code) {
            case 1 -> "one";
            case 2 -> { String s = "tw"; yield s + "o"; }
            default -> "other";
        };
        eq(label, "two", "switch-expr-yield");

        // text block (Java 15): line count, exact content, trailing-newline semantics
        String tb = """
                line1
                line2
                line3
                """;
        eq(tb.lines().count(), 3L, "textblock-lines");
        eq(tb, "line1\nline2\nline3\n", "textblock-exact");
        String noTrail = """
                a
                b""";
        eq(noTrail, "a\nb", "textblock-no-trailing-newline");
        // \s escape preserves trailing space; \ line-continuation joins
        String joined = """
                one \
                two""";
        eq(joined, "one two", "textblock-continuation");

        // var with inference
        var list = new ArrayList<String>();
        list.add("v");
        eq(list.get(0), "v", "var-inference");
        var sum = 0;
        for (var i = 1; i <= 5; i++) sum += i;
        eq(sum, 15, "var-loop");

        // enhanced instanceof in boolean expression
        Object o = "hello";
        boolean isLong = o instanceof String str && str.length() > 3;
        check(isLong, "instanceof-pattern-expr");
    }

    // ============================ Enums ============================
    static void testEnums() {
        eq(Color.values().length, 3, "enum-values-length");
        eq(Color.valueOf("GREEN"), Color.GREEN, "enum-valueOf");
        eq(Color.RED.ordinal(), 0, "enum-ordinal");
        eq(Color.BLUE.ordinal(), 2, "enum-ordinal-last");
        eq(Color.GREEN.name(), "GREEN", "enum-name");
        check(Color.RED.compareTo(Color.BLUE) < 0, "enum-compareTo");
        throwsEx(IllegalArgumentException.class, () -> Color.valueOf("PURPLE"), "enum-valueOf-bad");

        // enum in switch
        String hex = switch (Color.GREEN) {
            case RED -> "#f00";
            case GREEN -> "#0f0";
            case BLUE -> "#00f";
        };
        eq(hex, "#0f0", "enum-switch");

        // EnumMap ordering follows declaration order
        EnumMap<Color, Integer> em = new EnumMap<>(Color.class);
        em.put(Color.BLUE, 3); em.put(Color.RED, 1); em.put(Color.GREEN, 2);
        eq(new ArrayList<>(em.values()), List.of(1, 2, 3), "enummap-declaration-order");
    }

    // ============================ Objects utility ============================
    static void testObjects() {
        check(Objects.equals(null, null), "objects-equals-null-null");
        check(Objects.equals("a", "a"), "objects-equals-eq");
        check(!Objects.equals("a", null), "objects-equals-neq-null");
        eq(Objects.hashCode(null), 0, "objects-hashCode-null");
        eq(Objects.hash(1, 2, 3), 30817, "objects-hash-known");
        eq(Objects.hash(1, 2, 3), Objects.hash(1, 2, 3), "objects-hash-reproducible");
        check(Objects.isNull(null), "objects-isNull");
        check(Objects.nonNull("x"), "objects-nonNull");
        eq(Objects.toString(null, "def"), "def", "objects-toString-default");
        eq(Objects.toString(42, "def"), "42", "objects-toString-value");
        eq(Objects.requireNonNullElse(null, "fallback"), "fallback", "objects-requireNonNullElse");
        eq(Objects.requireNonNullElse("v", "fallback"), "v", "objects-requireNonNullElse-present");
        eq(Objects.requireNonNullElseGet(null, () -> "lazy"), "lazy", "objects-requireNonNullElseGet");
        check(Objects.compare(3, 5, Comparator.naturalOrder()) < 0, "objects-compare");
        throwsEx(NullPointerException.class, () -> Objects.requireNonNull(null, "must not be null"), "objects-requireNonNull-throws");
        eq(Objects.requireNonNull("ok"), "ok", "objects-requireNonNull-passthrough");
    }

    // ============================ Arrays utility ============================
    static void testArrays() {
        int[] a = {3, 1, 2, 5, 4};
        Arrays.sort(a);
        check(Arrays.equals(a, new int[]{1, 2, 3, 4, 5}), "arrays-sort");
        eq(Arrays.binarySearch(a, 4), 3, "arrays-binarySearch");
        eq(Arrays.stream(a).sum(), 15, "arrays-stream-sum");

        int[] cp = Arrays.copyOf(new int[]{1, 2}, 4);
        check(Arrays.equals(cp, new int[]{1, 2, 0, 0}), "arrays-copyOf");
        int[] range = Arrays.copyOfRange(new int[]{1, 2, 3, 4, 5}, 1, 4);
        check(Arrays.equals(range, new int[]{2, 3, 4}), "arrays-copyOfRange");

        int[] filled = new int[3];
        Arrays.fill(filled, 7);
        check(Arrays.equals(filled, new int[]{7, 7, 7}), "arrays-fill");

        int[] sa = new int[4];
        Arrays.setAll(sa, i -> i * i);
        check(Arrays.equals(sa, new int[]{0, 1, 4, 9}), "arrays-setAll");

        check(Arrays.compare(new int[]{1, 2}, new int[]{1, 3}) < 0, "arrays-compare");
        eq(Arrays.mismatch(new int[]{1, 2, 3}, new int[]{1, 2, 4}), 2, "arrays-mismatch");
        eq(Arrays.mismatch(new int[]{1, 2}, new int[]{1, 2}), -1, "arrays-mismatch-equal");

        // 2D deep equality / toString
        int[][] m1 = {{1, 2}, {3, 4}};
        int[][] m2 = {{1, 2}, {3, 4}};
        check(Arrays.deepEquals(m1, m2), "arrays-deepEquals");
        check(!Arrays.equals(m1, m2), "arrays-shallow-not-equal");
        eq(Arrays.deepToString(m1), "[[1, 2], [3, 4]]", "arrays-deepToString");

        // object sort with comparator
        String[] s = {"ccc", "a", "bb"};
        Arrays.sort(s, Comparator.comparingInt(String::length));
        check(Arrays.equals(s, new String[]{"a", "bb", "ccc"}), "arrays-sort-comparator");

        eq(Arrays.asList(1, 2, 3), List.of(1, 2, 3), "arrays-asList");
        eq(Arrays.hashCode(new int[]{1, 2, 3}), 30817, "arrays-hashCode");
    }

    // ============================ Collections utility ============================
    static void testCollectionsUtil() {
        List<Integer> l = new ArrayList<>(List.of(3, 1, 2));
        Collections.sort(l);
        eq(l, List.of(1, 2, 3), "collections-sort");
        Collections.reverse(l);
        eq(l, List.of(3, 2, 1), "collections-reverse");
        eq(Collections.max(l), 3, "collections-max");
        eq(Collections.min(l), 1, "collections-min");
        eq(Collections.frequency(List.of(1, 2, 2, 3, 2), 2), 3, "collections-frequency");
        check(Collections.disjoint(List.of(1, 2), List.of(3, 4)), "collections-disjoint");
        eq(Collections.nCopies(3, "x"), List.of("x", "x", "x"), "collections-nCopies");

        List<Integer> sorted = new ArrayList<>(List.of(1, 3, 5, 7, 9));
        eq(Collections.binarySearch(sorted, 7), 3, "collections-binarySearch");

        List<Integer> sw = new ArrayList<>(List.of(1, 2, 3, 4));
        Collections.swap(sw, 0, 3);
        eq(sw, List.of(4, 2, 3, 1), "collections-swap");

        List<Integer> rot = new ArrayList<>(List.of(1, 2, 3, 4, 5));
        Collections.rotate(rot, 2);
        eq(rot, List.of(4, 5, 1, 2, 3), "collections-rotate");

        List<String> rep = new ArrayList<>(List.of("a", "b", "a", "c"));
        Collections.replaceAll(rep, "a", "z");
        eq(rep, List.of("z", "b", "z", "c"), "collections-replaceAll");

        List<Integer> fil = new ArrayList<>(List.of(0, 0, 0));
        Collections.fill(fil, 9);
        eq(fil, List.of(9, 9, 9), "collections-fill");

        List<Integer> dst = new ArrayList<>();
        Collections.addAll(dst, 1, 2, 3);
        eq(dst, List.of(1, 2, 3), "collections-addAll");

        eq(Collections.emptyList(), List.of(), "collections-emptyList");
        eq(Collections.singletonList(5), List.of(5), "collections-singletonList");
        throwsEx(UnsupportedOperationException.class, () -> Collections.emptyList().add(1), "collections-emptyList-immutable");

        // synchronized view still behaves as a List
        List<Integer> sync = Collections.synchronizedList(new ArrayList<>(List.of(1, 2, 3)));
        eq(sync.size(), 3, "collections-synchronizedList");
    }

    // ============================ StringJoiner ============================
    static void testStringJoiner() {
        StringJoiner sj = new StringJoiner(", ", "[", "]");
        sj.add("a").add("b").add("c");
        eq(sj.toString(), "[a, b, c]", "stringjoiner-basic");

        StringJoiner empty = new StringJoiner(",");
        eq(empty.toString(), "", "stringjoiner-empty-default");
        empty.setEmptyValue("EMPTY");
        eq(empty.toString(), "EMPTY", "stringjoiner-setEmptyValue");

        StringJoiner a = new StringJoiner("-").add("1").add("2");
        StringJoiner b = new StringJoiner(":").add("3").add("4");
        a.merge(b);
        eq(a.toString(), "1-2-3:4", "stringjoiner-merge");

        eq(Stream.of("x", "y", "z").collect(() -> new StringJoiner("|"),
                StringJoiner::add, StringJoiner::merge).toString(), "x|y|z", "stringjoiner-collect");
    }
}
