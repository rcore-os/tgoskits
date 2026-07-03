package org.starry.dod;

import com.google.common.collect.*;
import com.google.common.base.*;
import com.google.common.primitives.*;
import com.google.common.hash.*;
import com.google.common.cache.*;
import java.util.*;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.TimeUnit;
import static java.nio.charset.StandardCharsets.UTF_8;

/**
 * Industrial-grade carpet for the Guava (com.google.common) library, version 33.2.1-jre.
 * Deterministic, offline, exact-equality assertions across the whole Guava surface:
 * immutable collections, multimaps, bimaps, tables, multisets, ranges/rangesets/rangemaps,
 * Optional, Preconditions, Lists/Sets/Maps/Iterables/Iterators utilities, Splitter/Joiner,
 * Hashing, CacheBuilder/LoadingCache, primitives, Strings, MoreObjects, Ordering, Stopwatch,
 * CharMatcher.
 */
public class GuavaCarpet {

    static int ok = 0;
    static int fail = 0;

    static void t(String name, boolean cond) {
        if (cond) ok++;
        else { fail++; System.out.println("FAIL " + name); }
    }

    static void eq(String name, Object actual, Object expected) {
        boolean c = (actual == null) ? (expected == null) : actual.equals(expected);
        if (c) ok++;
        else { fail++; System.out.println("FAIL " + name + " expected=[" + expected + "] actual=[" + actual + "]"); }
    }

    static void throwsType(String name, Class<? extends Throwable> ex, Runnable r) {
        try {
            r.run();
            fail++;
            System.out.println("FAIL " + name + " no exception (expected " + ex.getSimpleName() + ")");
        } catch (Throwable th) {
            if (ex.isInstance(th)) ok++;
            else { fail++; System.out.println("FAIL " + name + " wrong exception " + th.getClass().getName()); }
        }
    }

    // ---------------------------------------------------------------- immutable collections
    static void immutableCollections() {
        ImmutableList<Integer> il = ImmutableList.of(1, 2, 3, 4, 5);
        eq("immlist.size", il.size(), 5);
        eq("immlist.get", il.get(2), 3);
        eq("immlist.reverse", il.reverse(), ImmutableList.of(5, 4, 3, 2, 1));
        eq("immlist.subList", il.subList(1, 3), ImmutableList.of(2, 3));
        eq("immlist.indexOf", il.indexOf(4), 3);
        t("immlist.contains", il.contains(5));
        eq("immlist.asList", il.asList(), il);
        ImmutableList<String> ilb = ImmutableList.<String>builder()
                .add("a").add("b", "c").addAll(Arrays.asList("d", "e")).build();
        eq("immlist.builder", ilb, ImmutableList.of("a", "b", "c", "d", "e"));
        eq("immlist.copyOf", ImmutableList.copyOf(Arrays.asList(9, 8, 7)), ImmutableList.of(9, 8, 7));
        throwsType("immlist.immutable", UnsupportedOperationException.class, () -> { List<Integer> l = il; l.add(99); });
        throwsType("immlist.nullElem", NullPointerException.class, () -> ImmutableList.of("a", null));

        ImmutableSet<Integer> is = ImmutableSet.of(1, 2, 2, 3, 3, 3);
        eq("immset.size", is.size(), 3);
        eq("immset.asList", is.asList(), ImmutableList.of(1, 2, 3));
        t("immset.contains", is.contains(2));
        ImmutableSet<String> isb = ImmutableSet.<String>builder().add("x").add("x").add("y").build();
        eq("immset.builder.size", isb.size(), 2);

        ImmutableSortedSet<Integer> iss = ImmutableSortedSet.of(5, 3, 1, 4, 2);
        eq("immsortedset.first", iss.first(), 1);
        eq("immsortedset.last", iss.last(), 5);
        eq("immsortedset.asList", iss.asList(), ImmutableList.of(1, 2, 3, 4, 5));
        eq("immsortedset.headSet", iss.headSet(3).asList(), ImmutableList.of(1, 2));
        eq("immsortedset.tailSet", iss.tailSet(3).asList(), ImmutableList.of(3, 4, 5));
        eq("immsortedset.subSet", iss.subSet(2, 5).asList(), ImmutableList.of(2, 3, 4));
        eq("immsortedset.ceiling", iss.ceiling(3), 3);
        eq("immsortedset.floor", iss.floor(0), null);
        eq("immsortedset.descending", iss.descendingSet().asList(), ImmutableList.of(5, 4, 3, 2, 1));
        eq("immsortedset.comparator", iss.comparator(), Ordering.natural());
        ImmutableSortedSet<String> issr = ImmutableSortedSet.<String>reverseOrder().add("a").add("c").add("b").build();
        eq("immsortedset.reverseOrder", issr.asList(), ImmutableList.of("c", "b", "a"));

        ImmutableMap<String, Integer> im = ImmutableMap.of("one", 1, "two", 2, "three", 3);
        eq("immmap.size", im.size(), 3);
        eq("immmap.get", im.get("two"), 2);
        t("immmap.containsKey", im.containsKey("three"));
        eq("immmap.getOrDefault", im.getOrDefault("four", 0), 0);
        eq("immmap.keySet.order", Lists.newArrayList(im.keySet()), Arrays.asList("one", "two", "three"));
        ImmutableMap<String, Integer> imb = ImmutableMap.<String, Integer>builder().put("a", 1).put("b", 2).build();
        eq("immmap.builder", imb.get("b"), 2);
        throwsType("immmap.dupKey", IllegalArgumentException.class, () -> ImmutableMap.of("k", 1, "k", 2));

        ImmutableSortedMap<String, Integer> ism = ImmutableSortedMap.of("c", 3, "a", 1, "b", 2);
        eq("immsortedmap.firstKey", ism.firstKey(), "a");
        eq("immsortedmap.lastKey", ism.lastKey(), "c");
        eq("immsortedmap.keys.order", Lists.newArrayList(ism.keySet()), Arrays.asList("a", "b", "c"));
        eq("immsortedmap.headMap", Lists.newArrayList(ism.headMap("c").keySet()), Arrays.asList("a", "b"));

        ImmutableBiMap<String, Integer> ibm = ImmutableBiMap.of("a", 1, "b", 2, "c", 3);
        eq("immbimap.get", ibm.get("b"), 2);
        eq("immbimap.inverse.get", ibm.inverse().get(3), "c");
        t("immbimap.inverse.inverse", ibm.inverse().inverse() == ibm);
        throwsType("immbimap.dupVal", IllegalArgumentException.class, () -> ImmutableBiMap.of("a", 1, "b", 1));
    }

    // ---------------------------------------------------------------- multimaps
    static void multimaps() {
        ArrayListMultimap<String, Integer> alm = ArrayListMultimap.create();
        alm.put("a", 1); alm.put("a", 1); alm.put("a", 2); alm.put("b", 3);
        eq("alm.get.size", alm.get("a").size(), 3);
        eq("alm.size", alm.size(), 4);
        eq("alm.keySet.size", alm.keySet().size(), 2);
        t("alm.containsEntry", alm.containsEntry("a", 2));
        t("alm.containsKey", alm.containsKey("b"));
        eq("alm.keys.count", alm.keys().count("a"), 3);
        eq("alm.entries.size", alm.entries().size(), 4);
        eq("alm.values.size", alm.values().size(), 4);
        eq("alm.asMap.a", new ArrayList<>(alm.asMap().get("a")), Arrays.asList(1, 1, 2));

        HashMultimap<String, Integer> hmm = HashMultimap.create();
        hmm.put("a", 1); hmm.put("a", 1); hmm.put("a", 2);
        eq("hmm.get.size", hmm.get("a").size(), 2);
        eq("hmm.size", hmm.size(), 2);

        LinkedHashMultimap<String, Integer> lhm = LinkedHashMultimap.create();
        lhm.put("z", 1); lhm.put("a", 2); lhm.put("z", 3);
        eq("lhm.keySet.order", Lists.newArrayList(lhm.keySet()), Arrays.asList("z", "a"));
        eq("lhm.entries.order", lhm.size(), 3);

        TreeMultimap<String, Integer> tmm = TreeMultimap.create();
        tmm.put("b", 3); tmm.put("a", 2); tmm.put("a", 1); tmm.put("b", 1);
        eq("tmm.keySet.sorted", Lists.newArrayList(tmm.keySet()), Arrays.asList("a", "b"));
        eq("tmm.get.a.sorted", Lists.newArrayList(tmm.get("a")), Arrays.asList(1, 2));
        eq("tmm.get.b.sorted", Lists.newArrayList(tmm.get("b")), Arrays.asList(1, 3));

        ImmutableMultimap<String, Integer> imm = ImmutableMultimap.<String, Integer>builder()
                .put("a", 1).put("a", 2).putAll("b", 3, 4).build();
        eq("imm.size", imm.size(), 4);
        eq("imm.get.a", Lists.newArrayList(imm.get("a")), Arrays.asList(1, 2));
        ListMultimap<String, Integer> ilmm = ImmutableListMultimap.of("k", 1, "k", 2);
        eq("immlistmm.get", ilmm.get("k").size(), 2);

        ArrayListMultimap<String, Integer> rm = ArrayListMultimap.create();
        rm.put("x", 1); rm.put("x", 2);
        Collection<Integer> removed = rm.removeAll("x");
        eq("alm.removeAll", new ArrayList<>(removed), Arrays.asList(1, 2));
        t("alm.removeAll.empty", rm.get("x").isEmpty());
        rm.putAll("y", Arrays.asList(7, 8));
        rm.replaceValues("y", Arrays.asList(9));
        eq("alm.replaceValues", new ArrayList<>(rm.get("y")), Arrays.asList(9));

        HashBiMap<String, Integer> bm = HashBiMap.create();
        bm.put("a", 1); bm.put("b", 2);
        eq("bimap.get", bm.get("a"), 1);
        eq("bimap.inverse.get", bm.inverse().get(2), "b");
        t("bimap.inverse.inverse", bm.inverse().inverse() == bm);
        eq("bimap.values.size", bm.values().size(), 2);
        throwsType("bimap.dupVal", IllegalArgumentException.class, () -> {
            HashBiMap<String, Integer> b = HashBiMap.create(); b.put("a", 1); b.put("b", 1);
        });
        HashBiMap<String, Integer> bm2 = HashBiMap.create();
        bm2.put("a", 1); bm2.put("b", 2);
        bm2.forcePut("c", 1);
        eq("bimap.forcePut", bm2.inverse().get(1), "c");
        t("bimap.forcePut.removedOld", !bm2.containsKey("a"));
    }

    // ---------------------------------------------------------------- tables
    static void tables() {
        HashBasedTable<String, String, Integer> tbl = HashBasedTable.create();
        tbl.put("r1", "c1", 11); tbl.put("r1", "c2", 12); tbl.put("r2", "c1", 21);
        eq("table.get", tbl.get("r1", "c2"), 12);
        t("table.contains", tbl.contains("r2", "c1"));
        t("table.containsRow", tbl.containsRow("r1"));
        t("table.containsColumn", tbl.containsColumn("c2"));
        t("table.containsValue", tbl.containsValue(21));
        eq("table.size", tbl.size(), 3);
        eq("table.row.size", tbl.row("r1").size(), 2);
        eq("table.row.get", tbl.row("r1").get("c1"), 11);
        eq("table.column.size", tbl.column("c1").size(), 2);
        eq("table.column.get", tbl.column("c1").get("r2"), 21);
        eq("table.rowKeySet", new TreeSet<>(tbl.rowKeySet()), new TreeSet<>(Arrays.asList("r1", "r2")));
        eq("table.columnKeySet", new TreeSet<>(tbl.columnKeySet()), new TreeSet<>(Arrays.asList("c1", "c2")));
        eq("table.cellSet.size", tbl.cellSet().size(), 3);
        eq("table.values.size", tbl.values().size(), 3);
        eq("table.rowMap.size", tbl.rowMap().size(), 2);
        eq("table.columnMap.size", tbl.columnMap().size(), 2);
        tbl.remove("r1", "c1");
        eq("table.afterRemove.size", tbl.size(), 2);

        TreeBasedTable<String, String, Integer> ttbl = TreeBasedTable.create();
        ttbl.put("b", "y", 1); ttbl.put("a", "x", 2); ttbl.put("a", "z", 3);
        eq("treetable.rowKeySet.sorted", Lists.newArrayList(ttbl.rowKeySet()), Arrays.asList("a", "b"));
        eq("treetable.row.sorted", Lists.newArrayList(ttbl.row("a").keySet()), Arrays.asList("x", "z"));

        ImmutableTable<String, String, Integer> itbl = ImmutableTable.<String, String, Integer>builder()
                .put("r", "c", 5).build();
        eq("immtable.get", itbl.get("r", "c"), 5);
        eq("immtable.size", itbl.size(), 1);
    }

    // ---------------------------------------------------------------- multisets
    static void multisets() {
        HashMultiset<String> ms = HashMultiset.create();
        ms.add("apple"); ms.add("apple"); ms.add("banana"); ms.add("apple", 3);
        eq("multiset.count.apple", ms.count("apple"), 5);
        eq("multiset.count.banana", ms.count("banana"), 1);
        eq("multiset.count.absent", ms.count("cherry"), 0);
        eq("multiset.size", ms.size(), 6);
        eq("multiset.elementSet.size", ms.elementSet().size(), 2);
        eq("multiset.entrySet.size", ms.entrySet().size(), 2);
        ms.setCount("banana", 10);
        eq("multiset.setCount", ms.count("banana"), 10);
        ms.remove("apple", 2);
        eq("multiset.afterRemove", ms.count("apple"), 3);

        TreeMultiset<Integer> tms = TreeMultiset.create();
        tms.add(5); tms.add(1); tms.add(3); tms.add(1);
        eq("treemultiset.firstEntry.elem", tms.firstEntry().getElement(), 1);
        eq("treemultiset.firstEntry.count", tms.firstEntry().getCount(), 2);
        eq("treemultiset.lastEntry.elem", tms.lastEntry().getElement(), 5);
        eq("treemultiset.elementSet.sorted", Lists.newArrayList(tms.elementSet()), Arrays.asList(1, 3, 5));
        eq("treemultiset.size", tms.size(), 4);

        ImmutableMultiset<String> ims = ImmutableMultiset.of("a", "a", "b");
        eq("immmultiset.count", ims.count("a"), 2);
        eq("immmultiset.size", ims.size(), 3);
        eq("immmultiset.elementSet", ims.elementSet().size(), 2);
    }

    // ---------------------------------------------------------------- ranges / rangeset / rangemap
    static void ranges() {
        Range<Integer> r = Range.closed(1, 10);
        t("range.closed.low", r.contains(1));
        t("range.closed.high", r.contains(10));
        t("range.closed.mid", r.contains(5));
        t("range.closed.notContains", !r.contains(11));
        eq("range.lowerEndpoint", r.lowerEndpoint(), 1);
        eq("range.upperEndpoint", r.upperEndpoint(), 10);
        eq("range.lowerBoundType", r.lowerBoundType(), BoundType.CLOSED);
        eq("range.upperBoundType", r.upperBoundType(), BoundType.CLOSED);
        t("range.hasLowerBound", r.hasLowerBound());
        t("range.hasUpperBound", r.hasUpperBound());
        t("range.open.low", !Range.open(1, 10).contains(1));
        t("range.open.mid", Range.open(1, 10).contains(2));
        t("range.closedOpen.high", !Range.closedOpen(1, 10).contains(10));
        t("range.openClosed.high", Range.openClosed(1, 10).contains(10));
        t("range.atLeast", Range.atLeast(5).contains(1000000) && !Range.atLeast(5).contains(4));
        t("range.atMost", Range.atMost(5).contains(5) && !Range.atMost(5).contains(6));
        t("range.greaterThan", !Range.greaterThan(5).contains(5) && Range.greaterThan(5).contains(6));
        t("range.lessThan", !Range.lessThan(5).contains(5) && Range.lessThan(5).contains(4));
        t("range.singleton", Range.singleton(7).contains(7) && !Range.singleton(7).contains(8));
        t("range.empty", Range.closedOpen(5, 5).isEmpty());
        t("range.all", Range.<Integer>all().contains(Integer.MIN_VALUE));
        t("range.encloses", Range.closed(2, 8).encloses(Range.closed(3, 5)));
        t("range.notEncloses", !Range.closed(2, 8).encloses(Range.closed(3, 9)));
        eq("range.intersection", Range.closed(1, 5).intersection(Range.closed(3, 8)), Range.closed(3, 5));
        eq("range.span", Range.closed(1, 3).span(Range.closed(7, 9)), Range.closed(1, 9));
        t("range.isConnected", Range.closed(1, 5).isConnected(Range.closed(5, 10)));
        t("range.notConnected", !Range.closed(1, 3).isConnected(Range.closed(7, 9)));
        eq("range.gap", Range.closed(1, 3).gap(Range.closed(7, 9)), Range.open(3, 7));
        eq("range.equals", Range.closed(1, 5), Range.closed(1, 5));

        TreeRangeSet<Integer> rs = TreeRangeSet.create();
        rs.add(Range.closed(1, 5));
        rs.add(Range.closed(10, 15));
        t("rangeset.contains.3", rs.contains(3));
        t("rangeset.notContains.7", !rs.contains(7));
        eq("rangeset.asRanges.size", rs.asRanges().size(), 2);
        rs.add(Range.closed(4, 12));
        eq("rangeset.afterMerge.size", rs.asRanges().size(), 1);
        t("rangeset.afterMerge.contains7", rs.contains(7));
        eq("rangeset.span", rs.span(), Range.closed(1, 15));
        t("rangeset.encloses", rs.encloses(Range.closed(2, 3)));
        RangeSet<Integer> comp = rs.complement();
        t("rangeset.complement.contains0", comp.contains(0));
        t("rangeset.complement.notContains7", !comp.contains(7));
        rs.remove(Range.closed(5, 8));
        t("rangeset.afterRemove.notContains6", !rs.contains(6));
        t("rangeset.afterRemove.contains4", rs.contains(4));

        TreeRangeMap<Integer, String> rmap = TreeRangeMap.create();
        rmap.put(Range.closed(1, 10), "low");
        rmap.put(Range.closed(11, 20), "high");
        eq("rangemap.get.low", rmap.get(5), "low");
        eq("rangemap.get.high", rmap.get(15), "high");
        eq("rangemap.get.absent", rmap.get(25), null);
        eq("rangemap.span", rmap.span(), Range.closed(1, 20));
        eq("rangemap.asMapOfRanges.size", rmap.asMapOfRanges().size(), 2);
        rmap.put(Range.closed(5, 15), "mid");
        eq("rangemap.overwrite.5", rmap.get(5), "mid");
        eq("rangemap.overwrite.1", rmap.get(1), "low");
        eq("rangemap.overwrite.20", rmap.get(20), "high");
        eq("rangemap.overwrite.16", rmap.get(16), "high");
    }

    // ---------------------------------------------------------------- Optional + Preconditions
    static void optionalAndPreconditions() {
        com.google.common.base.Optional<String> opt = com.google.common.base.Optional.of("val");
        t("opt.isPresent", opt.isPresent());
        eq("opt.get", opt.get(), "val");
        eq("opt.or", opt.or("def"), "val");
        com.google.common.base.Optional<String> abs = com.google.common.base.Optional.absent();
        t("opt.absent.notPresent", !abs.isPresent());
        eq("opt.absent.or", abs.or("def"), "def");
        eq("opt.absent.orNull", abs.orNull(), null);
        com.google.common.base.Optional<String> fn = com.google.common.base.Optional.fromNullable(null);
        t("opt.fromNullable.null", !fn.isPresent());
        com.google.common.base.Optional<String> fn2 = com.google.common.base.Optional.fromNullable("x");
        t("opt.fromNullable.val", fn2.isPresent());
        eq("opt.transform", com.google.common.base.Optional.of("ab").transform((String s) -> s.length()).get(), 2);
        eq("opt.asSet.size", opt.asSet().size(), 1);
        eq("opt.absent.asSet.size", abs.asSet().size(), 0);
        throwsType("opt.absent.get.ise", IllegalStateException.class, () -> com.google.common.base.Optional.absent().get());
        throwsType("opt.of.null.npe", NullPointerException.class, () -> com.google.common.base.Optional.of(null));

        try {
            Preconditions.checkArgument(true);
            Preconditions.checkState(true);
            ok++;
        } catch (Exception e) { fail++; System.out.println("FAIL pre.pass"); }
        eq("pre.checkNotNull.return", Preconditions.checkNotNull("x"), "x");
        eq("pre.checkNotNull.msg.return", Preconditions.checkNotNull("y", "must not be null"), "y");
        eq("pre.checkElementIndex.return", Preconditions.checkElementIndex(2, 5), 2);
        eq("pre.checkPositionIndex.return", Preconditions.checkPositionIndex(5, 5), 5);
        throwsType("pre.checkArgument.false", IllegalArgumentException.class, () -> Preconditions.checkArgument(false, "bad"));
        throwsType("pre.checkNotNull.null", NullPointerException.class, () -> Preconditions.checkNotNull(null));
        throwsType("pre.checkState.false", IllegalStateException.class, () -> Preconditions.checkState(false));
        throwsType("pre.checkElementIndex.oob", IndexOutOfBoundsException.class, () -> Preconditions.checkElementIndex(5, 5));
        throwsType("pre.checkPositionIndex.oob", IndexOutOfBoundsException.class, () -> Preconditions.checkPositionIndex(6, 5));
    }

    // ---------------------------------------------------------------- Lists / Sets / Maps utilities
    static void collectionUtilities() {
        List<Integer> base = Arrays.asList(1, 2, 3, 4, 5);
        List<List<Integer>> lp = Lists.partition(base, 2);
        eq("lists.partition.count", lp.size(), 3);
        eq("lists.partition.0", lp.get(0), Arrays.asList(1, 2));
        eq("lists.partition.2", lp.get(2), Arrays.asList(5));
        eq("lists.reverse", Lists.reverse(base), Arrays.asList(5, 4, 3, 2, 1));
        eq("lists.transform", Lists.transform(Arrays.asList(1, 2, 3), (Integer x) -> x * 10), Arrays.asList(10, 20, 30));
        List<List<String>> cart = Lists.cartesianProduct(Arrays.asList("a", "b"), Arrays.asList("x", "y", "z"));
        eq("lists.cartesian.size", cart.size(), 6);
        eq("lists.cartesian.0", cart.get(0), Arrays.asList("a", "x"));
        eq("lists.charactersOf", Lists.charactersOf("abc"), Arrays.asList('a', 'b', 'c'));
        eq("lists.asList", Lists.asList("h", new String[]{"e", "y"}), Arrays.asList("h", "e", "y"));
        eq("lists.newArrayList", Lists.newArrayList(1, 2, 3), Arrays.asList(1, 2, 3));

        Set<Integer> s1 = Sets.newHashSet(1, 2, 3);
        Set<Integer> s2 = Sets.newHashSet(2, 3, 4);
        eq("sets.union", new TreeSet<>(Sets.union(s1, s2)), new TreeSet<>(Arrays.asList(1, 2, 3, 4)));
        eq("sets.intersection", new TreeSet<>(Sets.intersection(s1, s2)), new TreeSet<>(Arrays.asList(2, 3)));
        eq("sets.difference", new TreeSet<>(Sets.difference(s1, s2)), new TreeSet<>(Arrays.asList(1)));
        eq("sets.symDiff", new TreeSet<>(Sets.symmetricDifference(s1, s2)), new TreeSet<>(Arrays.asList(1, 4)));
        Set<Set<Integer>> ps = Sets.powerSet(ImmutableSet.of(1, 2, 3));
        eq("sets.powerSet.size", ps.size(), 8);
        t("sets.powerSet.hasEmpty", ps.contains(ImmutableSet.of()));
        t("sets.powerSet.hasFull", ps.contains(ImmutableSet.of(1, 2, 3)));
        Set<List<Integer>> scp = Sets.cartesianProduct(ImmutableSet.of(1, 2), ImmutableSet.of(3, 4));
        eq("sets.cartesian.size", scp.size(), 4);
        Set<Set<Integer>> combos = Sets.combinations(ImmutableSet.of(1, 2, 3, 4), 2);
        eq("sets.combinations.size", combos.size(), 6);

        Map<String, Integer> left = ImmutableMap.of("a", 1, "b", 2, "c", 3);
        Map<String, Integer> right = ImmutableMap.of("b", 2, "c", 4, "d", 5);
        MapDifference<String, Integer> diff = Maps.difference(left, right);
        t("maps.diff.notEqual", !diff.areEqual());
        eq("maps.diff.common", diff.entriesInCommon(), ImmutableMap.of("b", 2));
        eq("maps.diff.left", diff.entriesOnlyOnLeft(), ImmutableMap.of("a", 1));
        eq("maps.diff.right", diff.entriesOnlyOnRight(), ImmutableMap.of("d", 5));
        eq("maps.diff.differing.size", diff.entriesDiffering().size(), 1);
        t("maps.diff.differing.c", diff.entriesDiffering().containsKey("c"));
        Map<String, Integer> fk = Maps.filterKeys(left, (String k) -> !k.equals("b"));
        eq("maps.filterKeys", new TreeMap<>(fk), new TreeMap<>(ImmutableMap.of("a", 1, "c", 3)));
        Map<String, Integer> fv = Maps.filterValues(left, (Integer v) -> v > 1);
        eq("maps.filterValues", new TreeMap<>(fv), new TreeMap<>(ImmutableMap.of("b", 2, "c", 3)));
        Map<String, Integer> tv = Maps.transformValues(left, (Integer v) -> v * 100);
        eq("maps.transformValues", new TreeMap<>(tv), new TreeMap<>(ImmutableMap.of("a", 100, "b", 200, "c", 300)));
        Map<Integer, String> ui = Maps.uniqueIndex(Arrays.asList("a", "bb", "ccc"), (String s) -> s.length());
        eq("maps.uniqueIndex.size", ui.size(), 3);
        eq("maps.uniqueIndex.get", ui.get(2), "bb");
        Map<String, Integer> tm = Maps.toMap(Arrays.asList("x", "yy"), (String s) -> s.length());
        eq("maps.toMap", new TreeMap<>(tm), new TreeMap<>(ImmutableMap.of("x", 1, "yy", 2)));
    }

    // ---------------------------------------------------------------- Iterables / Iterators
    static void iterablesIterators() {
        eq("iter.getOnlyElement", Iterables.getOnlyElement(Arrays.asList("solo")), "solo");
        throwsType("iter.getOnly.multi", IllegalArgumentException.class, () -> Iterables.getOnlyElement(Arrays.asList(1, 2)));
        eq("iter.frequency", Iterables.frequency(Arrays.asList(1, 2, 2, 3, 2), 2), 3);
        eq("iter.getLast", Iterables.getLast(Arrays.asList(1, 2, 3)), 3);
        eq("iter.getFirst.default", Iterables.getFirst(new ArrayList<String>(), "def"), "def");
        eq("iter.size", Iterables.size(Arrays.asList(1, 2, 3, 4)), 4);
        t("iter.contains", Iterables.contains(Arrays.asList("x", "y"), "y"));
        eq("iter.get", Iterables.get(Arrays.asList("a", "b", "c"), 1), "b");
        eq("iter.concat", Lists.newArrayList(Iterables.concat(Arrays.asList(1, 2), Arrays.asList(3, 4))), Arrays.asList(1, 2, 3, 4));
        List<List<Integer>> parts = Lists.newArrayList(Iterables.partition(Arrays.asList(1, 2, 3, 4, 5), 2));
        eq("iter.partition.count", parts.size(), 3);
        eq("iter.partition.last", parts.get(2), Arrays.asList(5));
        t("iter.elementsEqual", Iterables.elementsEqual(Arrays.asList(1, 2), Arrays.asList(1, 2)));
        eq("iter.limit", Lists.newArrayList(Iterables.limit(Arrays.asList(1, 2, 3, 4), 2)), Arrays.asList(1, 2));
        eq("iter.transform", Lists.newArrayList(Iterables.transform(Arrays.asList(1, 2, 3), (Integer x) -> x + 100)), Arrays.asList(101, 102, 103));
        eq("iter.filter", Lists.newArrayList(Iterables.filter(Arrays.asList(1, 2, 3, 4, 5), (Integer x) -> x % 2 == 0)), Arrays.asList(2, 4));
        t("iter.isEmpty", Iterables.isEmpty(new ArrayList<String>()));

        eq("iters.size", Iterators.size(Arrays.asList(1, 2, 3).iterator()), 3);
        eq("iters.getOnly", Iterators.getOnlyElement(Arrays.asList("z").iterator()), "z");
        eq("iters.frequency", Iterators.frequency(Arrays.asList(1, 1, 1, 2).iterator(), 1), 3);
        Iterator<Integer> it = Arrays.asList(10, 20, 30, 40).iterator();
        int adv = Iterators.advance(it, 2);
        eq("iters.advance", adv, 2);
        eq("iters.afterAdvance", it.next(), 30);
        eq("iters.concat", Iterators.size(Iterators.concat(Arrays.asList(1, 2).iterator(), Arrays.asList(3).iterator())), 3);
        eq("iters.get", Iterators.get(Arrays.asList("p", "q", "r").iterator(), 2), "r");
    }

    // ---------------------------------------------------------------- Splitter / Joiner
    static void splitterJoiner() {
        eq("split.basic", Splitter.on(',').splitToList("a,b,c"), Arrays.asList("a", "b", "c"));
        eq("split.trim", Splitter.on(',').trimResults().splitToList(" a , b , c "), Arrays.asList("a", "b", "c"));
        eq("split.omitEmpty", Splitter.on(',').omitEmptyStrings().splitToList("a,,b,,,c"), Arrays.asList("a", "b", "c"));
        eq("split.trimOmit", Splitter.on(',').trimResults().omitEmptyStrings().splitToList(" a , , b "), Arrays.asList("a", "b"));
        eq("split.limit", Splitter.on(',').limit(2).splitToList("a,b,c,d"), Arrays.asList("a", "b,c,d"));
        eq("split.fixedLength", Splitter.fixedLength(2).splitToList("aabbcc"), Arrays.asList("aa", "bb", "cc"));
        eq("split.onString", Splitter.on("::").splitToList("a::b::c"), Arrays.asList("a", "b", "c"));
        Map<String, String> kv = Splitter.on(';').withKeyValueSeparator('=').split("a=1;b=2;c=3");
        eq("split.kv.size", kv.size(), 3);
        eq("split.kv.get", kv.get("b"), "2");

        eq("join.basic", Joiner.on(',').join(Arrays.asList("a", "b", "c")), "a,b,c");
        eq("join.varargs", Joiner.on('-').join(1, 2, 3), "1-2-3");
        eq("join.array", Joiner.on('|').join(new Object[]{"a", "b"}), "a|b");
        eq("join.skipNulls", Joiner.on(',').skipNulls().join(Arrays.asList("a", null, "b")), "a,b");
        eq("join.useForNull", Joiner.on(',').useForNull("N").join(Arrays.asList("a", null, "b")), "a,N,b");
        LinkedHashMap<String, Integer> jm = new LinkedHashMap<>();
        jm.put("a", 1); jm.put("b", 2);
        eq("join.kv", Joiner.on(';').withKeyValueSeparator('=').join(jm), "a=1;b=2");
        throwsType("join.null.npe", NullPointerException.class, () -> Joiner.on(',').join(Arrays.asList("a", null)));
    }

    // ---------------------------------------------------------------- Hashing
    static void hashing() {
        HashCode sha = Hashing.sha256().hashString("abc", UTF_8);
        eq("hash.sha256.abc", sha.toString(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        eq("hash.sha256.bits", Hashing.sha256().bits(), 256);
        eq("hash.md5.abc", Hashing.md5().hashString("abc", UTF_8).toString(), "900150983cd24fb0d6963f7d28e17f72");
        eq("hash.sha1.abc", Hashing.sha1().hashString("abc", UTF_8).toString(), "a9993e364706816aba3e25717850c26c9cd0d89d");

        java.util.zip.CRC32 crc = new java.util.zip.CRC32();
        crc.update("abc".getBytes(UTF_8));
        int crcExpected = (int) crc.getValue();
        eq("hash.crc32.matchesJdk", Hashing.crc32().hashString("abc", UTF_8).asInt(), crcExpected);
        eq("hash.crc32.bits", Hashing.crc32().bits(), 32);

        HashFunction mm = Hashing.murmur3_32_fixed();
        eq("hash.murmur.bits", mm.bits(), 32);
        eq("hash.murmur.det", mm.hashString("hello", UTF_8), mm.hashString("hello", UTF_8));
        t("hash.murmur.differ", !mm.hashString("a", UTF_8).equals(mm.hashString("b", UTF_8)));

        HashCode hc = Hashing.sha256().newHasher().putInt(42).putString("x", UTF_8).hash();
        eq("hash.hasher.det", hc, Hashing.sha256().newHasher().putInt(42).putString("x", UTF_8).hash());
        eq("hash.fromInt.asInt", HashCode.fromInt(12345).asInt(), 12345);
        eq("hash.fromLong.asLong", HashCode.fromLong(987654321987L).asLong(), 987654321987L);
        eq("hash.asBytes.len", HashCode.fromInt(1).asBytes().length, 4);

        HashCode a = HashCode.fromInt(1);
        HashCode b = HashCode.fromInt(2);
        HashCode c1 = Hashing.combineOrdered(Arrays.asList(a, b));
        HashCode c2 = Hashing.combineOrdered(Arrays.asList(b, a));
        t("hash.combineOrdered.differ", !c1.equals(c2));
        eq("hash.combineUnordered.same", Hashing.combineUnordered(Arrays.asList(a, b)),
                Hashing.combineUnordered(Arrays.asList(b, a)));

        int ch = Hashing.consistentHash(123456L, 10);
        t("hash.consistent.range", ch >= 0 && ch < 10);
        eq("hash.consistent.det", Hashing.consistentHash(123456L, 10), ch);
        eq("hash.consistent.one", Hashing.consistentHash(999L, 1), 0);
    }

    // ---------------------------------------------------------------- CacheBuilder / LoadingCache
    static void cache() {
        final AtomicInteger loads = new AtomicInteger(0);
        LoadingCache<Integer, Integer> c = CacheBuilder.newBuilder()
                .maximumSize(100).recordStats()
                .build(new CacheLoader<Integer, Integer>() {
                    public Integer load(Integer k) { loads.incrementAndGet(); return k * k; }
                });
        eq("cache.load", c.getUnchecked(4), 16);
        eq("cache.hit.value", c.getUnchecked(4), 16);
        eq("cache.loadsCount.1", loads.get(), 1);
        eq("cache.load2", c.getUnchecked(5), 25);
        eq("cache.size", c.size(), 2L);
        CacheStats stats = c.stats();
        eq("cache.stats.hitCount", stats.hitCount(), 1L);
        eq("cache.stats.missCount", stats.missCount(), 2L);
        eq("cache.stats.loadCount", stats.loadCount(), 2L);
        eq("cache.stats.requestCount", stats.requestCount(), 3L);
        t("cache.getIfPresent.has", c.getIfPresent(4) != null);
        t("cache.getIfPresent.absent", c.getIfPresent(999) == null);
        c.put(7, 49);
        eq("cache.put.get", c.getUnchecked(7), 49);
        c.invalidate(4);
        t("cache.invalidate", c.getIfPresent(4) == null);
        eq("cache.asMap.containsKey", c.asMap().containsKey(5), true);

        LoadingCache<Integer, Integer> small = CacheBuilder.newBuilder().maximumSize(2)
                .build(new CacheLoader<Integer, Integer>() {
                    public Integer load(Integer k) { return k; }
                });
        small.getUnchecked(1);
        small.getUnchecked(2);
        small.getUnchecked(3);
        small.cleanUp();
        t("cache.eviction", small.size() <= 2);
    }

    // ---------------------------------------------------------------- primitives
    static void primitives() {
        eq("ints.max", Ints.max(3, 7, 2, 9, 1), 9);
        eq("ints.min", Ints.min(3, 7, 2, 9, 1), 1);
        eq("ints.join", Ints.join(",", 1, 2, 3), "1,2,3");
        t("ints.contains", Ints.contains(new int[]{1, 2, 3}, 2));
        eq("ints.indexOf", Ints.indexOf(new int[]{5, 6, 7}, 7), 2);
        eq("ints.lastIndexOf", Ints.lastIndexOf(new int[]{1, 2, 1}, 1), 2);
        int[] cc = Ints.concat(new int[]{1, 2}, new int[]{3, 4});
        eq("ints.concat.len", cc.length, 4);
        eq("ints.concat.val", cc[2], 3);
        eq("ints.asList", Ints.asList(1, 2, 3), Arrays.asList(1, 2, 3));
        eq("ints.toArray", Ints.toArray(Arrays.asList(9, 8, 7))[0], 9);
        eq("ints.tryParse", Ints.tryParse("123"), 123);
        eq("ints.tryParse.bad", Ints.tryParse("12x"), null);
        eq("ints.saturated.max", Ints.saturatedCast(Long.MAX_VALUE), Integer.MAX_VALUE);
        eq("ints.saturated.min", Ints.saturatedCast(Long.MIN_VALUE), Integer.MIN_VALUE);
        eq("ints.constrain", Ints.constrainToRange(15, 0, 10), 10);

        eq("longs.max", Longs.max(1L, 5L, 3L), 5L);
        eq("longs.min", Longs.min(1L, 5L, 3L), 1L);
        eq("longs.join", Longs.join("-", 1L, 2L, 3L), "1-2-3");
        eq("longs.concat.len", Longs.concat(new long[]{1L}, new long[]{2L, 3L}).length, 3);
        eq("longs.tryParse", Longs.tryParse("9999999999"), 9999999999L);

        eq("doubles.max", Doubles.max(1.5, 2.5, 0.5), 2.5);
        eq("doubles.min", Doubles.min(1.5, 2.5, 0.5), 0.5);
        eq("doubles.join", Doubles.join(",", 1.0, 2.0), "1.0,2.0");
        eq("doubles.tryParse", Doubles.tryParse("3.14"), 3.14);
    }

    // ---------------------------------------------------------------- Strings + MoreObjects
    static void stringsMoreObjects() {
        eq("str.padStart", Strings.padStart("7", 3, '0'), "007");
        eq("str.padStart.noop", Strings.padStart("12345", 3, '0'), "12345");
        eq("str.padEnd", Strings.padEnd("ab", 5, '-'), "ab---");
        eq("str.repeat", Strings.repeat("ab", 3), "ababab");
        eq("str.repeat0", Strings.repeat("ab", 0), "");
        eq("str.commonPrefix", Strings.commonPrefix("abcdef", "abcxyz"), "abc");
        eq("str.commonSuffix", Strings.commonSuffix("123xyz", "456xyz"), "xyz");
        eq("str.nullToEmpty", Strings.nullToEmpty(null), "");
        eq("str.emptyToNull", Strings.emptyToNull(""), null);
        t("str.isNullOrEmpty.null", Strings.isNullOrEmpty(null));
        t("str.isNullOrEmpty.empty", Strings.isNullOrEmpty(""));
        t("str.isNullOrEmpty.false", !Strings.isNullOrEmpty("x"));
        eq("str.lenientFormat", Strings.lenientFormat("%s+%s=%s", 1, 2, 3), "1+2=3");

        eq("mo.firstNonNull.left", MoreObjects.firstNonNull("x", "d"), "x");
        eq("mo.firstNonNull.right", MoreObjects.firstNonNull(null, "d"), "d");
        eq("mo.toStringHelper", MoreObjects.toStringHelper("P").add("x", 1).add("y", "z").toString(), "P{x=1, y=z}");
        eq("mo.toStringHelper.withNull", MoreObjects.toStringHelper("P").add("x", 1).add("y", null).toString(), "P{x=1, y=null}");
        eq("mo.toStringHelper.omitNull", MoreObjects.toStringHelper("P").omitNullValues().add("x", 1).add("y", null).toString(), "P{x=1}");
    }

    // ---------------------------------------------------------------- Ordering
    static void ordering() {
        Ordering<Integer> nat = Ordering.natural();
        eq("ord.max", nat.max(Arrays.asList(3, 1, 4, 1, 5)), 5);
        eq("ord.min", nat.min(Arrays.asList(3, 1, 4, 1, 5)), 1);
        eq("ord.sorted", nat.sortedCopy(Arrays.asList(3, 1, 2)), Arrays.asList(1, 2, 3));
        eq("ord.reverse.sorted", nat.reverse().sortedCopy(Arrays.asList(1, 3, 2)), Arrays.asList(3, 2, 1));
        t("ord.isOrdered", nat.isOrdered(Arrays.asList(1, 2, 2, 3)));
        t("ord.isStrictlyOrdered.false", !nat.isStrictlyOrdered(Arrays.asList(1, 2, 2, 3)));
        List<String> withNull = Arrays.asList("b", null, "a");
        eq("ord.nullsFirst", Ordering.<String>natural().nullsFirst().sortedCopy(withNull), Arrays.asList(null, "a", "b"));
        eq("ord.nullsLast", Ordering.<String>natural().nullsLast().sortedCopy(withNull), Arrays.asList("a", "b", null));
        eq("ord.leastOf", nat.leastOf(Arrays.asList(5, 3, 1, 4, 2), 3), Arrays.asList(1, 2, 3));
        eq("ord.greatestOf", nat.greatestOf(Arrays.asList(5, 3, 1, 4, 2), 2), Arrays.asList(5, 4));
        Ordering<String> byLen = Ordering.<Integer>natural().onResultOf((String s) -> s.length());
        Ordering<String> comp = byLen.compound(Ordering.natural());
        eq("ord.compound", comp.sortedCopy(Arrays.asList("bb", "a", "cc", "b")), Arrays.asList("a", "b", "bb", "cc"));
        int cmp = Ordering.<Integer>natural().lexicographical().compare(Arrays.asList(1, 2), Arrays.asList(1, 3));
        t("ord.lexicographical", cmp < 0);
        eq("ord.immutableSortedCopy", nat.immutableSortedCopy(Arrays.asList(3, 1, 2)), ImmutableList.of(1, 2, 3));
    }

    // ---------------------------------------------------------------- Stopwatch
    static void stopwatch() {
        Stopwatch sw = Stopwatch.createUnstarted();
        t("sw.notRunning", !sw.isRunning());
        sw.start();
        t("sw.running", sw.isRunning());
        sw.stop();
        t("sw.stopped", !sw.isRunning());
        t("sw.elapsed", sw.elapsed(TimeUnit.NANOSECONDS) >= 0);
        Stopwatch sw2 = Stopwatch.createStarted();
        t("sw.created.running", sw2.isRunning());
        sw2.reset();
        t("sw.reset", !sw2.isRunning() && sw2.elapsed(TimeUnit.NANOSECONDS) == 0);
        throwsType("sw.doubleStart", IllegalStateException.class, () -> { Stopwatch s = Stopwatch.createStarted(); s.start(); });
    }

    // ---------------------------------------------------------------- CharMatcher
    static void charMatcher() {
        CharMatcher digit = CharMatcher.inRange('0', '9');
        eq("cm.digit.retain", digit.retainFrom("a1b2c3"), "123");
        eq("cm.digit.remove", digit.removeFrom("a1b2c3"), "abc");
        t("cm.digit.matchesAny", digit.matchesAnyOf("abc1"));
        t("cm.digit.matchesNone", digit.matchesNoneOf("abcd"));
        t("cm.digit.matchesAll", CharMatcher.inRange('0', '9').matchesAllOf("12345"));
        eq("cm.digit.count", digit.countIn("a1b22c333"), 6);
        eq("cm.is.indexIn", CharMatcher.is('l').indexIn("hello"), 2);
        eq("cm.is.lastIndexIn", CharMatcher.is('l').lastIndexIn("hello"), 3);

        CharMatcher ws = CharMatcher.whitespace();
        eq("cm.ws.trim", ws.trimFrom("  hi  "), "hi");
        eq("cm.ws.trimLeading", ws.trimLeadingFrom("  hi  "), "hi  ");
        eq("cm.ws.trimTrailing", ws.trimTrailingFrom("  hi  "), "  hi");
        eq("cm.ws.collapse", ws.collapseFrom("a   b    c", ' '), "a b c");
        eq("cm.ws.trimAndCollapse", ws.trimAndCollapseFrom("  a  b  ", ' '), "a b");

        CharMatcher vowel = CharMatcher.anyOf("aeiou");
        eq("cm.vowel.replace", vowel.replaceFrom("hello world", '*'), "h*ll* w*rld");
        eq("cm.vowel.count", vowel.countIn("hello world"), 3);

        t("cm.negate", CharMatcher.is('a').negate().matches('b'));
        t("cm.or", CharMatcher.is('a').or(CharMatcher.is('b')).matches('b'));
        t("cm.and", CharMatcher.inRange('a', 'z').and(CharMatcher.isNot('q')).matches('p'));
        t("cm.ascii", CharMatcher.ascii().matchesAllOf("hello"));
        t("cm.ascii.false", !CharMatcher.ascii().matches('é'));
        t("cm.none", CharMatcher.none().matchesNoneOf("anything"));
        t("cm.any", CharMatcher.any().matchesAllOf("anything"));
    }

    public static void main(String[] args) {
        immutableCollections();
        multimaps();
        tables();
        multisets();
        ranges();
        optionalAndPreconditions();
        collectionUtilities();
        iterablesIterators();
        splitterJoiner();
        hashing();
        cache();
        primitives();
        stringsMoreObjects();
        ordering();
        stopwatch();
        charMatcher();

        System.out.println("GUAVA_RESULT ok=" + ok + " fail=" + fail);
        if (fail == 0) {
            System.out.println("GUAVA_DONE");
        }
    }
}
