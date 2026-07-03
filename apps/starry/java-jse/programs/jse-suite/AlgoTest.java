import java.util.*;
import java.util.stream.*;
import java.util.function.*;

/* Carpet-grade coverage for the java.util collections framework, the
 * java.util.stream / java.util.function pipelines, and a broad classic
 * algorithm suite implemented over the JDK data structures.
 *
 * Every assertion checks an exact, deterministic value (==, equals, or a
 * known constant). No external I/O, no network, no JIT/timing dependence.
 * Pure in-heap computation: musl / StarryOS safe.
 *
 * Coverage matrix:
 *   java.util.Arrays      sort/parallelSort/binarySearch/fill/copyOf/copyOfRange/
 *                         equals/deepEquals/hashCode/deepHashCode/asList/stream/
 *                         setAll/compare/mismatch/deepToString
 *   java.util.Collections sort/binarySearch/reverse/max/min/frequency/nCopies/
 *                         empty + singleton + unmodifiable views/swap/rotate/
 *                         fill/disjoint/indexOfSubList/replaceAll/shuffle(seeded)
 *   List                  ArrayList / LinkedList(Deque) / List.of / subList /
 *                         ListIterator / removeIf / replaceAll
 *   Set                   HashSet / LinkedHashSet / TreeSet(NavigableSet)
 *   Map                   HashMap / LinkedHashMap(LRU) / TreeMap(NavigableMap)
 *   Queue/Deque           ArrayDeque(stack+queue) / PriorityQueue / Stack / Vector
 *   Comparator            naturalOrder/reverseOrder/comparing/thenComparing/
 *                         reversed/nullsFirst/nullsLast
 *   java.util.stream      Stream/IntStream/Collectors/Optional/summaryStatistics
 *   java.util.function    Function/BiFunction/Predicate/Supplier/Consumer/UnaryOp
 *   java.util.BitSet      set/clear/flip/and/or/xor/andNot/nextSetBit/cardinality
 *   java.util.Random      seeded determinism
 *   Integer/Long/Math     bit ops / overflow-exact / floorMod / gcd
 *   Algorithms            quicksort/mergesort/binsearch/quickselect/heap/
 *                         list-reverse/two-sum/coin-change/LCS/edit-distance/
 *                         knapsack/LIS/Kadane/BFS/DFS/Dijkstra/topo-sort/
 *                         union-find/Trie/KMP/sieve/valid-parens/LRU
 */
public class AlgoTest {
    static int ok = 0, fail = 0;
    static void check(boolean c, String name) {
        if (c) { ok++; } else { fail++; System.out.println("FAIL " + name); }
    }

    // ------------------------------------------------------------------
    // algorithm helpers
    // ------------------------------------------------------------------
    static void qsort(int[] a, int lo, int hi) {
        if (lo >= hi) return;
        int p = a[(lo + hi) >>> 1], i = lo, j = hi;
        while (i <= j) {
            while (a[i] < p) i++;
            while (a[j] > p) j--;
            if (i <= j) { int t = a[i]; a[i] = a[j]; a[j] = t; i++; j--; }
        }
        qsort(a, lo, j); qsort(a, i, hi);
    }
    static int[] mergeSort(int[] a) {
        if (a.length <= 1) return a;
        int mid = a.length / 2;
        int[] l = mergeSort(Arrays.copyOfRange(a, 0, mid));
        int[] r = mergeSort(Arrays.copyOfRange(a, mid, a.length));
        int[] out = new int[a.length];
        int i = 0, j = 0, k = 0;
        while (i < l.length && j < r.length) out[k++] = (l[i] <= r[j]) ? l[i++] : r[j++];
        while (i < l.length) out[k++] = l[i++];
        while (j < r.length) out[k++] = r[j++];
        return out;
    }
    static int bsearch(int[] a, int key) {
        int lo = 0, hi = a.length - 1;
        while (lo <= hi) { int m = (lo + hi) >>> 1; if (a[m] == key) return m; else if (a[m] < key) lo = m + 1; else hi = m - 1; }
        return -1;
    }
    static int quickselect(int[] a, int k) { // k-th smallest, 0-based, mutates copy
        int[] c = a.clone();
        int lo = 0, hi = c.length - 1;
        while (lo < hi) {
            int p = c[(lo + hi) >>> 1], i = lo, j = hi;
            while (i <= j) {
                while (c[i] < p) i++;
                while (c[j] > p) j--;
                if (i <= j) { int t = c[i]; c[i] = c[j]; c[j] = t; i++; j--; }
            }
            if (k <= j) hi = j; else if (k >= i) lo = i; else break;
        }
        return c[k];
    }
    static class Node { int v; Node next; Node(int v) { this.v = v; } }
    static Node reverse(Node h) { Node prev = null; while (h != null) { Node n = h.next; h.next = prev; prev = h; h = n; } return prev; }
    static int[] twoSum(int[] a, int target) {
        Map<Integer, Integer> m = new HashMap<>();
        for (int i = 0; i < a.length; i++) { if (m.containsKey(target - a[i])) return new int[]{m.get(target - a[i]), i}; m.put(a[i], i); }
        return new int[]{-1, -1};
    }
    static int coinChange(int[] coins, int amount) {
        int[] dp = new int[amount + 1]; Arrays.fill(dp, amount + 1); dp[0] = 0;
        for (int c : coins) for (int x = c; x <= amount; x++) dp[x] = Math.min(dp[x], dp[x - c] + 1);
        return dp[amount] > amount ? -1 : dp[amount];
    }
    static int lcs(String a, String b) {
        int[][] dp = new int[a.length() + 1][b.length() + 1];
        for (int i = 1; i <= a.length(); i++)
            for (int j = 1; j <= b.length(); j++)
                dp[i][j] = a.charAt(i - 1) == b.charAt(j - 1) ? dp[i - 1][j - 1] + 1 : Math.max(dp[i - 1][j], dp[i][j - 1]);
        return dp[a.length()][b.length()];
    }
    static int editDistance(String a, String b) {
        int[][] dp = new int[a.length() + 1][b.length() + 1];
        for (int i = 0; i <= a.length(); i++) dp[i][0] = i;
        for (int j = 0; j <= b.length(); j++) dp[0][j] = j;
        for (int i = 1; i <= a.length(); i++)
            for (int j = 1; j <= b.length(); j++)
                dp[i][j] = a.charAt(i - 1) == b.charAt(j - 1) ? dp[i - 1][j - 1]
                        : 1 + Math.min(dp[i - 1][j - 1], Math.min(dp[i - 1][j], dp[i][j - 1]));
        return dp[a.length()][b.length()];
    }
    static int knapsack(int[] w, int[] v, int cap) {
        int[] dp = new int[cap + 1];
        for (int i = 0; i < w.length; i++)
            for (int c = cap; c >= w[i]; c--)
                dp[c] = Math.max(dp[c], dp[c - w[i]] + v[i]);
        return dp[cap];
    }
    static int lis(int[] a) { // O(n log n) length of longest strictly increasing subseq
        int[] tails = new int[a.length]; int size = 0;
        for (int x : a) {
            int lo = 0, hi = size;
            while (lo < hi) { int m = (lo + hi) >>> 1; if (tails[m] < x) lo = m + 1; else hi = m; }
            tails[lo] = x; if (lo == size) size++;
        }
        return size;
    }
    static int kadane(int[] a) {
        int best = a[0], cur = a[0];
        for (int i = 1; i < a.length; i++) { cur = Math.max(a[i], cur + a[i]); best = Math.max(best, cur); }
        return best;
    }
    static int bfs(List<List<Integer>> g, int s, int t) {
        int[] dist = new int[g.size()]; Arrays.fill(dist, -1); dist[s] = 0;
        Deque<Integer> q = new ArrayDeque<>(); q.add(s);
        while (!q.isEmpty()) { int u = q.poll(); for (int v : g.get(u)) if (dist[v] < 0) { dist[v] = dist[u] + 1; q.add(v); } }
        return dist[t];
    }
    static int dfsCount(List<List<Integer>> g, int s) {
        boolean[] seen = new boolean[g.size()];
        Deque<Integer> st = new ArrayDeque<>(); st.push(s); int n = 0;
        while (!st.isEmpty()) { int u = st.pop(); if (seen[u]) continue; seen[u] = true; n++; for (int v : g.get(u)) if (!seen[v]) st.push(v); }
        return n;
    }
    static int[] dijkstra(int n, List<int[]>[] adj, int src) {
        int[] dist = new int[n]; Arrays.fill(dist, Integer.MAX_VALUE); dist[src] = 0;
        PriorityQueue<int[]> pq = new PriorityQueue<>((x, y) -> Integer.compare(x[1], y[1]));
        pq.add(new int[]{src, 0});
        while (!pq.isEmpty()) {
            int[] cur = pq.poll(); int u = cur[0], d = cur[1];
            if (d > dist[u]) continue;
            for (int[] e : adj[u]) { int v = e[0], w = e[1]; if (dist[u] != Integer.MAX_VALUE && dist[u] + w < dist[v]) { dist[v] = dist[u] + w; pq.add(new int[]{v, dist[v]}); } }
        }
        return dist;
    }
    @SuppressWarnings("unchecked")
    static List<Integer> topo(int n, List<Integer>[] adj) {
        int[] indeg = new int[n];
        for (List<Integer> l : adj) for (int v : l) indeg[v]++;
        Deque<Integer> q = new ArrayDeque<>();
        for (int i = 0; i < n; i++) if (indeg[i] == 0) q.add(i);
        List<Integer> order = new ArrayList<>();
        while (!q.isEmpty()) { int u = q.poll(); order.add(u); for (int v : adj[u]) if (--indeg[v] == 0) q.add(v); }
        return order;
    }
    static class DSU {
        int[] p, r;
        DSU(int n) { p = new int[n]; r = new int[n]; for (int i = 0; i < n; i++) p[i] = i; }
        int find(int x) { return p[x] == x ? x : (p[x] = find(p[x])); }
        boolean union(int a, int b) {
            int x = find(a), y = find(b);
            if (x == y) return false;
            if (r[x] < r[y]) { int t = x; x = y; y = t; }
            p[y] = x; if (r[x] == r[y]) r[x]++;
            return true;
        }
    }
    static class Trie {
        Trie[] ch = new Trie[26]; boolean end;
        void insert(String w) { Trie t = this; for (char c : w.toCharArray()) { int i = c - 'a'; if (t.ch[i] == null) t.ch[i] = new Trie(); t = t.ch[i]; } t.end = true; }
        Trie find(String w) { Trie t = this; for (char c : w.toCharArray()) { int i = c - 'a'; if (t.ch[i] == null) return null; t = t.ch[i]; } return t; }
        boolean search(String w) { Trie t = find(w); return t != null && t.end; }
        boolean startsWith(String p) { return find(p) != null; }
    }
    static int kmp(String text, String pat) {
        int n = pat.length(); int[] lps = new int[n];
        for (int i = 1, len = 0; i < n; ) {
            if (pat.charAt(i) == pat.charAt(len)) lps[i++] = ++len;
            else if (len > 0) len = lps[len - 1];
            else lps[i++] = 0;
        }
        for (int i = 0, j = 0; i < text.length(); ) {
            if (text.charAt(i) == pat.charAt(j)) { i++; j++; if (j == n) return i - j; }
            else if (j > 0) j = lps[j - 1];
            else i++;
        }
        return -1;
    }
    static int countPrimes(int n) {
        if (n < 3) return 0;
        boolean[] comp = new boolean[n]; int c = 0;
        for (int i = 2; i < n; i++) if (!comp[i]) { c++; for (long j = (long) i * i; j < n; j += i) comp[(int) j] = true; }
        return c;
    }
    static boolean validParen(String s) {
        Deque<Character> st = new ArrayDeque<>();
        for (char c : s.toCharArray()) {
            if (c == '(' || c == '[' || c == '{') st.push(c);
            else { if (st.isEmpty()) return false; char o = st.pop(); if ((c == ')' && o != '(') || (c == ']' && o != '[') || (c == '}' && o != '{')) return false; }
        }
        return st.isEmpty();
    }
    static int gcd(int a, int b) { while (b != 0) { int t = b; b = a % b; a = t; } return a; }
    static class LRU<K, V> extends LinkedHashMap<K, V> {
        final int cap;
        LRU(int cap) { super(16, 0.75f, true); this.cap = cap; }
        protected boolean removeEldestEntry(Map.Entry<K, V> e) { return size() > cap; }
    }

    // ------------------------------------------------------------------
    public static void main(String[] args) {
        testArrays();
        testCollectionsUtil();
        testLists();
        testSets();
        testMaps();
        testQueuesDeques();
        testComparators();
        testStreams();
        testOptional();
        testFunctional();
        testBitSet();
        testRandom();
        testBitMathOps();
        testStringBuilder();
        testExceptions();
        testAlgorithms();

        System.out.println("ALGO_RESULT ok=" + ok + " fail=" + fail);
        if (fail == 0) System.out.println("ALGO_DONE");
    }

    // ------------------------------------------------------------------
    static void testArrays() {
        int[] a = {5, 2, 9, 1, 5, 6, 3};
        int[] b = a.clone();
        Arrays.sort(b);
        check(Arrays.equals(b, new int[]{1, 2, 3, 5, 5, 6, 9}), "arrays-sort");
        check(Arrays.binarySearch(b, 6) == 5, "arrays-binarySearch-found");
        check(Arrays.binarySearch(b, 4) == -4, "arrays-binarySearch-insertionpoint");

        int[] range = {9, 8, 7, 6, 5};
        Arrays.sort(range, 1, 4); // sort [8,7,6] -> [6,7,8]
        check(Arrays.equals(range, new int[]{9, 6, 7, 8, 5}), "arrays-sort-range");

        int[] filled = new int[5];
        Arrays.fill(filled, 7);
        check(Arrays.equals(filled, new int[]{7, 7, 7, 7, 7}), "arrays-fill");
        Arrays.fill(filled, 1, 4, 0);
        check(Arrays.equals(filled, new int[]{7, 0, 0, 0, 7}), "arrays-fill-range");

        check(Arrays.equals(Arrays.copyOf(new int[]{1, 2, 3}, 5), new int[]{1, 2, 3, 0, 0}), "arrays-copyOf-grow");
        check(Arrays.equals(Arrays.copyOf(new int[]{1, 2, 3}, 2), new int[]{1, 2}), "arrays-copyOf-shrink");
        check(Arrays.equals(Arrays.copyOfRange(new int[]{1, 2, 3, 4, 5}, 1, 4), new int[]{2, 3, 4}), "arrays-copyOfRange");

        int[] sq = new int[5];
        Arrays.setAll(sq, i -> i * i);
        check(Arrays.equals(sq, new int[]{0, 1, 4, 9, 16}), "arrays-setAll");

        check(Arrays.compare(new int[]{1, 2, 3}, new int[]{1, 2, 4}) < 0, "arrays-compare-lt");
        check(Arrays.compare(new int[]{1, 2, 3}, new int[]{1, 2, 3}) == 0, "arrays-compare-eq");
        check(Arrays.compare(new int[]{1, 2, 3}, new int[]{1, 2}) > 0, "arrays-compare-prefix");
        check(Arrays.mismatch(new int[]{1, 2, 3}, new int[]{1, 2, 4}) == 2, "arrays-mismatch");
        check(Arrays.mismatch(new int[]{1, 2, 3}, new int[]{1, 2, 3}) == -1, "arrays-mismatch-equal");

        int[][] m1 = {{1, 2}, {3, 4}}, m2 = {{1, 2}, {3, 4}};
        check(!Arrays.equals(m1, m2), "arrays-equals-shallow-2d");
        check(Arrays.deepEquals(m1, m2), "arrays-deepEquals-2d");
        check(Arrays.hashCode(new int[]{1, 2, 3}) == Arrays.hashCode(new int[]{1, 2, 3}), "arrays-hashCode");
        check(Arrays.deepHashCode(m1) == Arrays.deepHashCode(m2), "arrays-deepHashCode");
        check(Arrays.deepToString(m1).equals("[[1, 2], [3, 4]]"), "arrays-deepToString");
        check(Arrays.toString(new int[]{1, 2, 3}).equals("[1, 2, 3]"), "arrays-toString");

        List<Integer> al = Arrays.asList(1, 2, 3);
        check(al.size() == 3 && al.get(1) == 2, "arrays-asList");
        check(Arrays.stream(new int[]{1, 2, 3, 4}).sum() == 10, "arrays-stream-sum");

        Integer[] objs = {3, 1, 2};
        Arrays.sort(objs, Comparator.reverseOrder());
        check(Arrays.equals(objs, new Integer[]{3, 2, 1}), "arrays-sort-comparator");

        // parallelSort on a small array (below parallel threshold -> sequential, deterministic)
        int[] ps = {4, 1, 3, 2};
        Arrays.parallelSort(ps);
        check(Arrays.equals(ps, new int[]{1, 2, 3, 4}), "arrays-parallelSort");

        long[] longs = {3L, 1L, 2L};
        Arrays.sort(longs);
        check(longs[0] == 1L && longs[2] == 3L, "arrays-sort-long");
        double[] ds = {2.5, 1.1, 3.3};
        Arrays.sort(ds);
        check(ds[0] == 1.1 && ds[2] == 3.3, "arrays-sort-double");
    }

    // ------------------------------------------------------------------
    static void testCollectionsUtil() {
        List<Integer> l = new ArrayList<>(List.of(5, 3, 1, 4, 2));
        Collections.sort(l);
        check(l.equals(List.of(1, 2, 3, 4, 5)), "collections-sort");
        Collections.sort(l, Comparator.reverseOrder());
        check(l.equals(List.of(5, 4, 3, 2, 1)), "collections-sort-comparator");
        Collections.reverse(l);
        check(l.equals(List.of(1, 2, 3, 4, 5)), "collections-reverse");

        check(Collections.binarySearch(l, 3) == 2, "collections-binarySearch");
        check(Collections.max(l) == 5, "collections-max");
        check(Collections.min(l) == 1, "collections-min");
        check(Collections.max(List.of(1, 2, 3), Comparator.reverseOrder()) == 1, "collections-max-comparator");

        check(Collections.frequency(List.of(1, 1, 2, 1, 3), 1) == 3, "collections-frequency");
        check(Collections.nCopies(3, "x").equals(List.of("x", "x", "x")), "collections-nCopies");
        check(Collections.disjoint(List.of(1, 2), List.of(3, 4)), "collections-disjoint-true");
        check(!Collections.disjoint(List.of(1, 2), List.of(2, 3)), "collections-disjoint-false");
        check(Collections.indexOfSubList(List.of(1, 2, 3, 4, 5), List.of(3, 4)) == 2, "collections-indexOfSubList");

        List<Integer> rot = new ArrayList<>(List.of(1, 2, 3, 4, 5));
        Collections.rotate(rot, 2);
        check(rot.equals(List.of(4, 5, 1, 2, 3)), "collections-rotate");
        List<Integer> sw = new ArrayList<>(List.of(1, 2, 3));
        Collections.swap(sw, 0, 2);
        check(sw.equals(List.of(3, 2, 1)), "collections-swap");
        List<Integer> fil = new ArrayList<>(List.of(1, 2, 3));
        Collections.fill(fil, 9);
        check(fil.equals(List.of(9, 9, 9)), "collections-fill");
        List<String> rep = new ArrayList<>(List.of("a", "b", "a", "c"));
        Collections.replaceAll(rep, "a", "z");
        check(rep.equals(List.of("z", "b", "z", "c")), "collections-replaceAll");

        check(Collections.emptyList().isEmpty(), "collections-emptyList");
        check(Collections.singletonList(42).equals(List.of(42)), "collections-singletonList");
        check(Collections.emptyMap().isEmpty() && Collections.emptySet().isEmpty(), "collections-empty-mapset");

        // seeded shuffle: reproducible + permutation-preserving
        List<Integer> s1 = new ArrayList<>(List.of(1, 2, 3, 4, 5, 6, 7, 8));
        List<Integer> s2 = new ArrayList<>(s1);
        Collections.shuffle(s1, new Random(12345));
        Collections.shuffle(s2, new Random(12345));
        check(s1.equals(s2), "collections-shuffle-reproducible");
        List<Integer> sorted = new ArrayList<>(s1);
        Collections.sort(sorted);
        check(sorted.equals(List.of(1, 2, 3, 4, 5, 6, 7, 8)), "collections-shuffle-permutation");

        // unmodifiable view rejects mutation
        List<Integer> um = Collections.unmodifiableList(new ArrayList<>(List.of(1, 2, 3)));
        boolean thrown = false;
        try { um.add(4); } catch (UnsupportedOperationException e) { thrown = true; }
        check(thrown, "collections-unmodifiable-throws");
    }

    // ------------------------------------------------------------------
    static void testLists() {
        List<Integer> al = new ArrayList<>();
        for (int i = 0; i < 5; i++) al.add(i * 10);
        check(al.equals(List.of(0, 10, 20, 30, 40)), "arraylist-add");
        al.set(2, 99);
        check(al.get(2) == 99, "arraylist-set");
        al.add(1, 5);
        check(al.equals(List.of(0, 5, 10, 99, 30, 40)), "arraylist-add-index");
        al.remove(Integer.valueOf(99));
        check(al.equals(List.of(0, 5, 10, 30, 40)), "arraylist-remove-object");
        al.remove(0);
        check(al.equals(List.of(5, 10, 30, 40)), "arraylist-remove-index");
        check(al.indexOf(30) == 2 && al.lastIndexOf(40) == 3, "arraylist-indexOf");
        check(al.contains(10) && !al.contains(999), "arraylist-contains");
        check(al.subList(1, 3).equals(List.of(10, 30)), "arraylist-subList");

        List<Integer> ri = new ArrayList<>(List.of(1, 2, 3, 4, 5, 6));
        ri.removeIf(x -> x % 2 == 0);
        check(ri.equals(List.of(1, 3, 5)), "list-removeIf");
        List<Integer> ra = new ArrayList<>(List.of(1, 2, 3));
        ra.replaceAll(x -> x * x);
        check(ra.equals(List.of(1, 4, 9)), "list-replaceAll");

        List<Integer> retain = new ArrayList<>(List.of(1, 2, 3, 4, 5));
        retain.retainAll(List.of(2, 4, 6));
        check(retain.equals(List.of(2, 4)), "list-retainAll");
        List<Integer> rem = new ArrayList<>(List.of(1, 2, 3, 4, 5));
        rem.removeAll(List.of(2, 4));
        check(rem.equals(List.of(1, 3, 5)), "list-removeAll");

        // ListIterator forward/back + set + add
        List<Integer> li = new ArrayList<>(List.of(1, 2, 3));
        ListIterator<Integer> it = li.listIterator();
        int sum = 0; while (it.hasNext()) sum += it.next();
        check(sum == 6, "listiterator-forward");
        int back = 0; while (it.hasPrevious()) back += it.previous();
        check(back == 6, "listiterator-backward");
        ListIterator<Integer> it2 = li.listIterator();
        it2.next(); it2.set(10);
        check(li.get(0) == 10, "listiterator-set");

        // LinkedList as Deque
        LinkedList<Integer> ll = new LinkedList<>();
        ll.addFirst(2); ll.addFirst(1); ll.addLast(3);
        check(ll.equals(List.of(1, 2, 3)), "linkedlist-deque-order");
        check(ll.peekFirst() == 1 && ll.peekLast() == 3, "linkedlist-peek");
        check(ll.pollFirst() == 1 && ll.pollLast() == 3, "linkedlist-poll");
        LinkedList<Integer> dl = new LinkedList<>(List.of(1, 2, 3));
        Iterator<Integer> di = dl.descendingIterator();
        check(di.next() == 3 && di.next() == 2 && di.next() == 1, "linkedlist-descendingIterator");

        // immutable List.of
        check(List.of(1, 2, 3).size() == 3, "list-of-size");
    }

    // ------------------------------------------------------------------
    static void testSets() {
        Set<Integer> hs = new HashSet<>(List.of(1, 2, 3, 2, 1));
        check(hs.size() == 3, "hashset-dedupe");
        check(hs.contains(2) && !hs.contains(9), "hashset-contains");
        hs.add(4); hs.remove(1);
        check(hs.size() == 3 && hs.contains(4) && !hs.contains(1), "hashset-add-remove");
        Set<Integer> retain = new HashSet<>(Set.of(1, 2, 3, 4));
        retain.retainAll(Set.of(2, 4, 6));
        check(retain.equals(Set.of(2, 4)), "hashset-retainAll");

        // LinkedHashSet preserves insertion order
        Set<Integer> lhs = new LinkedHashSet<>();
        lhs.add(3); lhs.add(1); lhs.add(2); lhs.add(1);
        check(new ArrayList<>(lhs).equals(List.of(3, 1, 2)), "linkedhashset-order");

        // TreeSet / NavigableSet
        TreeSet<Integer> ts = new TreeSet<>(List.of(10, 20, 30, 40, 50));
        check(ts.first() == 10 && ts.last() == 50, "treeset-first-last");
        check(ts.ceiling(25) == 30 && ts.floor(25) == 20, "treeset-ceiling-floor");
        check(ts.higher(30) == 40 && ts.lower(30) == 20, "treeset-higher-lower");
        check(ts.headSet(30).equals(Set.of(10, 20)), "treeset-headSet");
        check(ts.tailSet(30).equals(Set.of(30, 40, 50)), "treeset-tailSet");
        check(ts.subSet(20, 50).equals(Set.of(20, 30, 40)), "treeset-subSet");
        check(new ArrayList<>(ts.descendingSet()).equals(List.of(50, 40, 30, 20, 10)), "treeset-descendingSet");
        check(ts.pollFirst() == 10 && ts.pollLast() == 50, "treeset-poll");
        TreeSet<String> tss = new TreeSet<>(Comparator.reverseOrder());
        tss.addAll(List.of("a", "c", "b"));
        check(tss.first().equals("c") && tss.last().equals("a"), "treeset-comparator");
    }

    // ------------------------------------------------------------------
    static void testMaps() {
        Map<String, Integer> hm = new HashMap<>();
        hm.put("a", 1); hm.put("b", 2);
        check(hm.get("a") == 1 && hm.getOrDefault("z", -1) == -1, "hashmap-get-default");
        hm.putIfAbsent("a", 99); hm.putIfAbsent("c", 3);
        check(hm.get("a") == 1 && hm.get("c") == 3, "hashmap-putIfAbsent");
        hm.merge("a", 10, Integer::sum);
        check(hm.get("a") == 11, "hashmap-merge");
        hm.compute("b", (k, v) -> v * 5);
        check(hm.get("b") == 10, "hashmap-compute");
        hm.computeIfAbsent("d", k -> 4);
        check(hm.get("d") == 4, "hashmap-computeIfAbsent");
        hm.computeIfPresent("d", (k, v) -> v + 100);
        check(hm.get("d") == 104, "hashmap-computeIfPresent");
        hm.replace("c", 33);
        check(hm.get("c") == 33, "hashmap-replace");
        check(hm.containsKey("a") && hm.containsValue(10), "hashmap-contains");
        check(hm.keySet().size() == hm.size() && hm.values().size() == hm.size(), "hashmap-views");
        int entrySum = 0; for (Map.Entry<String, Integer> e : hm.entrySet()) entrySum += e.getValue();
        check(entrySum == 11 + 10 + 33 + 104, "hashmap-entrySet-sum");
        // forEach
        int[] acc = {0}; hm.forEach((k, v) -> acc[0] += v);
        check(acc[0] == 11 + 10 + 33 + 104, "hashmap-forEach");

        // LinkedHashMap insertion order
        Map<Integer, String> lhm = new LinkedHashMap<>();
        lhm.put(3, "c"); lhm.put(1, "a"); lhm.put(2, "b");
        check(new ArrayList<>(lhm.keySet()).equals(List.of(3, 1, 2)), "linkedhashmap-order");

        // TreeMap / NavigableMap
        TreeMap<Integer, String> tm = new TreeMap<>();
        tm.put(30, "c"); tm.put(10, "a"); tm.put(20, "b"); tm.put(40, "d");
        check(tm.firstKey() == 10 && tm.lastKey() == 40, "treemap-first-last-key");
        check(tm.ceilingKey(15) == 20 && tm.floorKey(15) == 10, "treemap-ceiling-floor");
        check(tm.higherKey(20) == 30 && tm.lowerKey(20) == 10, "treemap-higher-lower");
        check(tm.firstEntry().getValue().equals("a") && tm.lastEntry().getValue().equals("d"), "treemap-entries");
        check(tm.headMap(30).keySet().equals(Set.of(10, 20)), "treemap-headMap");
        check(tm.tailMap(30).keySet().equals(Set.of(30, 40)), "treemap-tailMap");
        check(tm.subMap(20, 40).keySet().equals(Set.of(20, 30)), "treemap-subMap");
        check(new ArrayList<>(tm.descendingKeySet()).equals(List.of(40, 30, 20, 10)), "treemap-descendingKeySet");
        check(tm.pollFirstEntry().getKey() == 10 && tm.pollLastEntry().getKey() == 40, "treemap-poll-entries");

        // immutable Map.of
        Map<String, Integer> mof = Map.of("x", 1, "y", 2);
        check(mof.get("x") == 1 && mof.size() == 2, "map-of");
    }

    // ------------------------------------------------------------------
    static void testQueuesDeques() {
        // ArrayDeque as stack (LIFO)
        Deque<Integer> stack = new ArrayDeque<>();
        stack.push(1); stack.push(2); stack.push(3);
        check(stack.pop() == 3 && stack.pop() == 2 && stack.peek() == 1, "arraydeque-stack");
        // ArrayDeque as queue (FIFO)
        Deque<Integer> queue = new ArrayDeque<>();
        queue.offer(1); queue.offer(2); queue.offer(3);
        check(queue.poll() == 1 && queue.poll() == 2 && queue.peek() == 3, "arraydeque-queue");
        Deque<Integer> dq = new ArrayDeque<>();
        dq.addFirst(2); dq.addLast(3); dq.addFirst(1);
        check(dq.peekFirst() == 1 && dq.peekLast() == 3, "arraydeque-bothends");

        // PriorityQueue min-heap default
        PriorityQueue<Integer> min = new PriorityQueue<>();
        for (int x : new int[]{4, 1, 7, 3, 9, 2}) min.add(x);
        check(min.poll() == 1 && min.poll() == 2 && min.poll() == 3, "pq-min-heap");
        // PriorityQueue max-heap
        PriorityQueue<Integer> max = new PriorityQueue<>(Comparator.reverseOrder());
        for (int x : new int[]{4, 1, 7, 3, 9, 2}) max.add(x);
        check(max.poll() == 9 && max.poll() == 7 && max.poll() == 4, "pq-max-heap");
        // PriorityQueue custom comparator (by string length)
        PriorityQueue<String> bylen = new PriorityQueue<>(Comparator.comparingInt(String::length));
        bylen.addAll(List.of("ccc", "a", "bb"));
        check(bylen.poll().equals("a") && bylen.poll().equals("bb"), "pq-comparator");

        // legacy Stack / Vector
        Stack<Integer> s = new Stack<>();
        s.push(10); s.push(20); s.push(30);
        check(s.peek() == 30 && s.pop() == 30 && s.size() == 2, "stack-legacy");
        check(s.search(10) == 2, "stack-search");
        check(!s.empty(), "stack-empty");
        Vector<Integer> v = new Vector<>(List.of(1, 2, 3));
        v.add(4); v.insertElementAt(0, 0);
        check(v.firstElement() == 0 && v.lastElement() == 4 && v.size() == 5, "vector-legacy");
    }

    // ------------------------------------------------------------------
    static void testComparators() {
        check(Comparator.<Integer>naturalOrder().compare(1, 2) < 0, "comparator-naturalOrder");
        check(Comparator.<Integer>reverseOrder().compare(1, 2) > 0, "comparator-reverseOrder");

        List<String> words = new ArrayList<>(List.of("bb", "a", "ccc", "dd"));
        words.sort(Comparator.comparingInt(String::length).thenComparing(Comparator.naturalOrder()));
        check(words.equals(List.of("a", "bb", "dd", "ccc")), "comparator-comparingInt-thenComparing");

        List<String> rev = new ArrayList<>(List.of("a", "b", "c"));
        rev.sort(Comparator.<String>naturalOrder().reversed());
        check(rev.equals(List.of("c", "b", "a")), "comparator-reversed");

        List<String> nf = new ArrayList<>(Arrays.asList("b", null, "a"));
        nf.sort(Comparator.nullsFirst(Comparator.naturalOrder()));
        check(nf.get(0) == null && nf.get(1).equals("a"), "comparator-nullsFirst");
        List<String> nl = new ArrayList<>(Arrays.asList("b", null, "a"));
        nl.sort(Comparator.nullsLast(Comparator.naturalOrder()));
        check(nl.get(2) == null && nl.get(0).equals("a"), "comparator-nullsLast");

        // comparing with key extractor + reversed
        List<int[]> pts = new ArrayList<>();
        pts.add(new int[]{1, 5}); pts.add(new int[]{3, 1}); pts.add(new int[]{2, 9});
        pts.sort(Comparator.comparingInt(p -> p[1]));
        check(pts.get(0)[1] == 1 && pts.get(2)[1] == 9, "comparator-keyExtractor");
    }

    // ------------------------------------------------------------------
    static void testStreams() {
        check(IntStream.rangeClosed(1, 5).sum() == 15, "intstream-rangeClosed-sum");
        check(IntStream.range(0, 5).sum() == 10, "intstream-range-sum");
        check(IntStream.rangeClosed(1, 5).reduce(1, (x, y) -> x * y) == 120, "intstream-reduce-factorial");
        check(Stream.of(1, 2, 3, 4, 5, 6).filter(x -> x % 2 == 0).mapToInt(Integer::intValue).sum() == 12, "stream-filter-map-sum");
        check(Stream.of(3, 1, 4, 1, 5, 9, 2).sorted().collect(Collectors.toList()).equals(List.of(1, 1, 2, 3, 4, 5, 9)), "stream-sorted");
        check(Stream.of(1, 2, 2, 3, 3, 3).distinct().count() == 3, "stream-distinct");
        check(Stream.of(1, 2, 3, 4, 5).limit(3).collect(Collectors.toList()).equals(List.of(1, 2, 3)), "stream-limit");
        check(Stream.of(1, 2, 3, 4, 5).skip(2).collect(Collectors.toList()).equals(List.of(3, 4, 5)), "stream-skip");
        check(Stream.of(1, 2, 3, 4).reduce(0, Integer::sum) == 10, "stream-reduce");
        check(Stream.of(1, 2, 3).map(x -> x * x).collect(Collectors.toList()).equals(List.of(1, 4, 9)), "stream-map");

        // flatMap
        check(Stream.of(List.of(1, 2), List.of(3, 4), List.of(5)).flatMap(List::stream).count() == 5, "stream-flatMap");

        // iterate / generate
        check(Stream.iterate(1, x -> x * 2).limit(5).collect(Collectors.toList()).equals(List.of(1, 2, 4, 8, 16)), "stream-iterate");
        check(Stream.generate(() -> 7).limit(3).collect(Collectors.toList()).equals(List.of(7, 7, 7)), "stream-generate");

        // min/max/count/anyMatch/allMatch/noneMatch
        check(Stream.of(5, 3, 8, 1).max(Comparator.naturalOrder()).get() == 8, "stream-max");
        check(Stream.of(5, 3, 8, 1).min(Comparator.naturalOrder()).get() == 1, "stream-min");
        check(Stream.of(2, 4, 6).allMatch(x -> x % 2 == 0), "stream-allMatch");
        check(Stream.of(1, 2, 3).anyMatch(x -> x == 2), "stream-anyMatch");
        check(Stream.of(1, 3, 5).noneMatch(x -> x % 2 == 0), "stream-noneMatch");
        check(Stream.of(1, 2, 3, 4).findFirst().get() == 1, "stream-findFirst");

        // IntStream statistics
        IntSummaryStatistics st = IntStream.of(1, 2, 3, 4, 5).summaryStatistics();
        check(st.getCount() == 5 && st.getSum() == 15 && st.getMin() == 1 && st.getMax() == 5 && st.getAverage() == 3.0, "intstream-summaryStatistics");
        check(IntStream.of(5, 3, 8, 1).max().getAsInt() == 8, "intstream-max");
        check(IntStream.range(1, 4).boxed().collect(Collectors.toList()).equals(List.of(1, 2, 3)), "intstream-boxed");
        check(IntStream.rangeClosed(1, 5).average().getAsDouble() == 3.0, "intstream-average");

        // Collectors
        check(Stream.of("a", "b", "c").collect(Collectors.joining(",", "[", "]")).equals("[a,b,c]"), "collectors-joining");
        check(Stream.of("x", "y", "z").collect(Collectors.joining()).equals("xyz"), "collectors-joining-plain");
        check(Stream.of(1, 2, 3, 4).collect(Collectors.summingInt(x -> x)) == 10, "collectors-summingInt");
        check(Stream.of(1, 2, 3, 4).collect(Collectors.averagingInt(x -> x)) == 2.5, "collectors-averagingInt");
        check(Stream.of(1, 2, 3).collect(Collectors.counting()) == 3, "collectors-counting");

        Map<Integer, Long> grouped = Stream.of(1, 2, 3, 4, 5, 6, 7).collect(Collectors.groupingBy(x -> x % 3, Collectors.counting()));
        check(grouped.get(0) == 2 && grouped.get(1) == 3 && grouped.get(2) == 2, "collectors-groupingBy");
        Map<Boolean, List<Integer>> part = Stream.of(1, 2, 3, 4, 5, 6).collect(Collectors.partitioningBy(x -> x % 2 == 0));
        check(part.get(true).equals(List.of(2, 4, 6)) && part.get(false).equals(List.of(1, 3, 5)), "collectors-partitioningBy");
        Map<String, Integer> tomap = Stream.of("a", "bb", "ccc").collect(Collectors.toMap(s -> s, String::length));
        check(tomap.get("bb") == 2 && tomap.get("ccc") == 3, "collectors-toMap");
        Set<Integer> toset = Stream.of(1, 2, 2, 3).collect(Collectors.toSet());
        check(toset.equals(Set.of(1, 2, 3)), "collectors-toSet");
        TreeSet<Integer> tocoll = Stream.of(3, 1, 2).collect(Collectors.toCollection(TreeSet::new));
        check(tocoll.first() == 1 && tocoll.last() == 3, "collectors-toCollection");
        List<Integer> mapped = Stream.of("a", "bb", "ccc").collect(Collectors.mapping(String::length, Collectors.toList()));
        check(mapped.equals(List.of(1, 2, 3)), "collectors-mapping");

        // Stream.toList (Java 16+)
        check(Stream.of(1, 2, 3).map(x -> x + 1).toList().equals(List.of(2, 3, 4)), "stream-toList");

        // peek side effect (deterministic count)
        int[] cnt = {0};
        long c = Stream.of(1, 2, 3).peek(x -> cnt[0]++).count();
        check(c == 3, "stream-count");
    }

    // ------------------------------------------------------------------
    static void testOptional() {
        check(Optional.of(5).isPresent() && !Optional.of(5).isEmpty(), "optional-of");
        check(Optional.empty().isEmpty(), "optional-empty");
        check(!Optional.ofNullable(null).isPresent(), "optional-ofNullable-null");
        check(Optional.of(5).map(x -> x * 2).get() == 10, "optional-map");
        check(Optional.of(5).filter(x -> x > 10).isEmpty(), "optional-filter-out");
        check(Optional.of(5).filter(x -> x > 1).get() == 5, "optional-filter-keep");
        check(Optional.empty().orElse(99).equals(99), "optional-orElse");
        check(Optional.<Integer>empty().orElseGet(() -> 7) == 7, "optional-orElseGet");
        check(Optional.of(3).flatMap(x -> Optional.of(x + 1)).get() == 4, "optional-flatMap");
        int[] acc = {0};
        Optional.of(10).ifPresent(x -> acc[0] = x);
        Optional.<Integer>empty().ifPresent(x -> acc[0] = -1);
        check(acc[0] == 10, "optional-ifPresent");
        boolean thrown = false;
        try { Optional.empty().get(); } catch (NoSuchElementException e) { thrown = true; }
        check(thrown, "optional-get-empty-throws");
        check(OptionalInt.of(42).getAsInt() == 42, "optionalint");
    }

    // ------------------------------------------------------------------
    static void testFunctional() {
        Function<Integer, Integer> inc = x -> x + 1;
        Function<Integer, Integer> dbl = x -> x * 2;
        check(inc.andThen(dbl).apply(3) == 8, "function-andThen");   // (3+1)*2
        check(inc.compose(dbl).apply(3) == 7, "function-compose");   // (3*2)+1
        check(Function.<Integer>identity().apply(5) == 5, "function-identity");

        BiFunction<Integer, Integer, Integer> add = (a, b) -> a + b;
        check(add.apply(3, 4) == 7, "bifunction-apply");

        Predicate<Integer> even = x -> x % 2 == 0;
        Predicate<Integer> pos = x -> x > 0;
        check(even.and(pos).test(4) && !even.and(pos).test(-2), "predicate-and");
        check(even.or(pos).test(3), "predicate-or");
        check(even.negate().test(3), "predicate-negate");

        Supplier<String> sup = () -> "hi";
        check(sup.get().equals("hi"), "supplier-get");

        int[] box = {0};
        Consumer<Integer> con = x -> box[0] += x;
        con.andThen(x -> box[0] += x * 10).accept(2);
        check(box[0] == 2 + 20, "consumer-andThen");

        UnaryOperator<String> up = String::toUpperCase;
        check(up.apply("abc").equals("ABC"), "unaryoperator");
        BinaryOperator<Integer> bmax = BinaryOperator.maxBy(Comparator.naturalOrder());
        check(bmax.apply(3, 7) == 7, "binaryoperator-maxBy");

        ToIntFunction<String> len = String::length;
        check(len.applyAsInt("hello") == 5, "tointfunction");
        IntBinaryOperator mul = (a, b) -> a * b;
        check(mul.applyAsInt(6, 7) == 42, "intbinaryoperator");
        IntPredicate ip = x -> x > 0;
        check(ip.test(5) && !ip.test(-1), "intpredicate");
    }

    // ------------------------------------------------------------------
    static void testBitSet() {
        BitSet bs = new BitSet();
        bs.set(1); bs.set(3); bs.set(5);
        check(bs.get(3) && !bs.get(2), "bitset-set-get");
        check(bs.cardinality() == 3, "bitset-cardinality");
        check(bs.nextSetBit(2) == 3, "bitset-nextSetBit");
        check(bs.nextClearBit(1) == 2, "bitset-nextClearBit");
        check(bs.length() == 6, "bitset-length");
        bs.clear(3);
        check(!bs.get(3) && bs.cardinality() == 2, "bitset-clear");
        bs.flip(3);
        check(bs.get(3), "bitset-flip");

        BitSet x = new BitSet(); x.set(0); x.set(1); x.set(2); // 111
        BitSet y = new BitSet(); y.set(1); y.set(2); y.set(3); // 1110
        BitSet and = (BitSet) x.clone(); and.and(y);
        check(and.get(1) && and.get(2) && !and.get(0) && !and.get(3), "bitset-and");
        BitSet or = (BitSet) x.clone(); or.or(y);
        check(or.cardinality() == 4, "bitset-or");
        BitSet xor = (BitSet) x.clone(); xor.xor(y);
        check(xor.get(0) && xor.get(3) && !xor.get(1), "bitset-xor");
        BitSet andNot = (BitSet) x.clone(); andNot.andNot(y);
        check(andNot.get(0) && !andNot.get(1) && !andNot.get(2), "bitset-andNot");
        check(x.intersects(y), "bitset-intersects");

        // set range
        BitSet r = new BitSet(); r.set(2, 5);
        check(r.get(2) && r.get(4) && !r.get(5) && r.cardinality() == 3, "bitset-set-range");
    }

    // ------------------------------------------------------------------
    static void testRandom() {
        // Random is a specified LCG: same seed -> identical sequence.
        Random a = new Random(987654321L);
        Random b = new Random(987654321L);
        boolean same = true;
        for (int i = 0; i < 50; i++) if (a.nextInt(1000) != b.nextInt(1000)) same = false;
        check(same, "random-seed-reproducible");

        Random c = new Random(42);
        boolean inRange = true;
        for (int i = 0; i < 100; i++) { int x = c.nextInt(10); if (x < 0 || x >= 10) inRange = false; }
        check(inRange, "random-bounded");

        Random d = new Random(7);
        boolean dblRange = true;
        for (int i = 0; i < 50; i++) { double x = d.nextDouble(); if (x < 0.0 || x >= 1.0) dblRange = false; }
        check(dblRange, "random-nextDouble-range");

        // ints stream reproducible
        int[] s1 = new Random(5).ints(20, 0, 100).toArray();
        int[] s2 = new Random(5).ints(20, 0, 100).toArray();
        check(Arrays.equals(s1, s2), "random-ints-stream-reproducible");
    }

    // ------------------------------------------------------------------
    static void testBitMathOps() {
        check(Integer.bitCount(7) == 3 && Integer.bitCount(255) == 8, "integer-bitCount");
        check(Integer.highestOneBit(100) == 64, "integer-highestOneBit");
        check(Integer.lowestOneBit(12) == 4, "integer-lowestOneBit");
        check(Integer.numberOfLeadingZeros(1) == 31, "integer-numberOfLeadingZeros");
        check(Integer.numberOfTrailingZeros(8) == 3, "integer-numberOfTrailingZeros");
        check(Integer.reverse(1) == Integer.MIN_VALUE, "integer-reverse");
        check(Integer.reverseBytes(0x01020304) == 0x04030201, "integer-reverseBytes");
        check(Integer.rotateLeft(0x80000000, 1) == 1, "integer-rotateLeft");
        check(Integer.rotateRight(1, 1) == Integer.MIN_VALUE, "integer-rotateRight");
        check(Integer.toBinaryString(10).equals("1010"), "integer-toBinaryString");
        check(Integer.toHexString(255).equals("ff"), "integer-toHexString");
        check(Integer.parseInt("ff", 16) == 255, "integer-parseInt-radix");
        check(Integer.parseInt("-42") == -42, "integer-parseInt");
        check(Integer.compare(3, 5) < 0 && Integer.max(3, 5) == 5 && Integer.min(3, 5) == 3, "integer-compare-minmax");
        check(Integer.signum(-7) == -1 && Integer.signum(0) == 0, "integer-signum");

        check(Long.bitCount(0xFFL) == 8, "long-bitCount");
        check(Long.numberOfTrailingZeros(0L) == 64, "long-numberOfTrailingZeros-zero");
        check(Long.highestOneBit(1000L) == 512L, "long-highestOneBit");

        check(Math.floorMod(-7, 3) == 2 && Math.floorMod(7, 3) == 1, "math-floorMod");
        check(Math.floorDiv(-7, 3) == -3, "math-floorDiv");
        check(gcd(48, 36) == 12 && gcd(17, 5) == 1, "gcd-euclid");
        check(Math.abs(-5) == 5 && Math.max(3, 7) == 7 && Math.min(3, 7) == 3, "math-abs-minmax");
        check(Math.pow(2, 10) == 1024.0, "math-pow");
        check((long) Math.sqrt(144.0) == 12, "math-sqrt");

        // exact arithmetic overflow
        boolean of = false;
        try { Math.addExact(Integer.MAX_VALUE, 1); } catch (ArithmeticException e) { of = true; }
        check(of, "math-addExact-overflow");
        boolean mof = false;
        try { Math.multiplyExact(Integer.MAX_VALUE, 2); } catch (ArithmeticException e) { mof = true; }
        check(mof, "math-multiplyExact-overflow");
        check(Math.addExact(2, 3) == 5 && Math.subtractExact(10, 4) == 6, "math-exact-ok");
    }

    // ------------------------------------------------------------------
    static void testStringBuilder() {
        StringBuilder sb = new StringBuilder();
        sb.append("hello").append(' ').append("world").append(42).append(true);
        check(sb.toString().equals("hello world42true"), "sb-append-chain");
        check(sb.length() == 17, "sb-length");
        check(sb.charAt(0) == 'h', "sb-charAt");
        check(sb.indexOf("world") == 6, "sb-indexOf");

        StringBuilder ins = new StringBuilder("hello");
        ins.insert(0, ">>");
        check(ins.toString().equals(">>hello"), "sb-insert");
        ins.reverse();
        check(ins.toString().equals("olleh>>"), "sb-reverse");

        StringBuilder del = new StringBuilder("abcdef");
        del.delete(1, 3);
        check(del.toString().equals("adef"), "sb-delete");
        del.deleteCharAt(0);
        check(del.toString().equals("def"), "sb-deleteCharAt");
        del.replace(0, 1, "XYZ");
        check(del.toString().equals("XYZef"), "sb-replace");
        del.setCharAt(0, 'A');
        check(del.charAt(0) == 'A', "sb-setCharAt");
        del.setLength(2);
        check(del.toString().equals("AY"), "sb-setLength");

        StringBuffer sbuf = new StringBuffer("buf");
        sbuf.append("fer").insert(0, "x");
        check(sbuf.toString().equals("xbuffer"), "stringbuffer");
    }

    // ------------------------------------------------------------------
    static void testExceptions() {
        boolean aioobe = false;
        try { int[] z = new int[2]; int y = z[5]; check(y == 0, "unreachable"); }
        catch (ArrayIndexOutOfBoundsException e) { aioobe = true; }
        check(aioobe, "exc-aioobe");

        boolean nfe = false;
        try { Integer.parseInt("not-a-number"); } catch (NumberFormatException e) { nfe = true; }
        check(nfe, "exc-numberFormat");

        boolean npe = false;
        try { String s = null; s.length(); } catch (NullPointerException e) { npe = true; }
        check(npe, "exc-npe");

        boolean cme = false;
        List<Integer> l = new ArrayList<>(List.of(1, 2, 3));
        try { for (Integer x : l) { if (x == 2) l.add(99); } } catch (ConcurrentModificationException e) { cme = true; }
        check(cme, "exc-concurrentModification");

        boolean nse = false;
        try { new ArrayList<Integer>().iterator().next(); } catch (NoSuchElementException e) { nse = true; }
        check(nse, "exc-noSuchElement");

        boolean uoe = false;
        try { List.of(1, 2, 3).set(0, 9); } catch (UnsupportedOperationException e) { uoe = true; }
        check(uoe, "exc-unsupportedOperation");

        boolean ae = false;
        try { int x = 5 / (args_len_zero() ); } catch (ArithmeticException e) { ae = true; }
        check(ae, "exc-arithmetic-divzero");

        boolean cce = false;
        try { Object o = "str"; Integer i = (Integer) o; } catch (ClassCastException e) { cce = true; }
        check(cce, "exc-classCast");

        // try-with-resources determinism via custom AutoCloseable
        StringBuilder log = new StringBuilder();
        try (AutoCloseable r1 = () -> log.append("c1"); AutoCloseable r2 = () -> log.append("c2")) {
            log.append("body");
        } catch (Exception e) { /* none */ }
        check(log.toString().equals("bodyc2c1"), "try-with-resources-order");
    }
    static int args_len_zero() { return 0; }

    // ------------------------------------------------------------------
    @SuppressWarnings("unchecked")
    static void testAlgorithms() {
        int[] a = {5, 2, 9, 1, 5, 6, 3};
        int[] qs = a.clone(); qsort(qs, 0, qs.length - 1);
        check(Arrays.equals(qs, new int[]{1, 2, 3, 5, 5, 6, 9}), "algo-quicksort");
        check(Arrays.equals(mergeSort(a.clone()), new int[]{1, 2, 3, 5, 5, 6, 9}), "algo-mergesort");
        check(bsearch(qs, 6) == 5 && bsearch(qs, 4) == -1, "algo-bsearch");
        check(quickselect(new int[]{7, 3, 1, 9, 5}, 0) == 1 && quickselect(new int[]{7, 3, 1, 9, 5}, 4) == 9 && quickselect(new int[]{7, 3, 1, 9, 5}, 2) == 5, "algo-quickselect");

        Node h = new Node(1); h.next = new Node(2); h.next.next = new Node(3);
        Node r = reverse(h);
        check(r.v == 3 && r.next.v == 2 && r.next.next.v == 1, "algo-reverse-list");

        check(Arrays.equals(twoSum(new int[]{2, 7, 11, 15}, 9), new int[]{0, 1}), "algo-two-sum");
        check(coinChange(new int[]{1, 2, 5}, 11) == 3, "algo-coin-change");
        check(coinChange(new int[]{2}, 3) == -1, "algo-coin-change-impossible");
        check(lcs("abcde", "ace") == 3, "algo-lcs");
        check(editDistance("horse", "ros") == 3 && editDistance("intention", "execution") == 5, "algo-edit-distance");
        check(knapsack(new int[]{1, 3, 4, 5}, new int[]{1, 4, 5, 7}, 7) == 9, "algo-knapsack");
        check(lis(new int[]{10, 9, 2, 5, 3, 7, 101, 18}) == 4, "algo-lis");
        check(kadane(new int[]{-2, 1, -3, 4, -1, 2, 1, -5, 4}) == 6, "algo-kadane");

        List<List<Integer>> g = new ArrayList<>();
        for (int i = 0; i < 6; i++) g.add(new ArrayList<>());
        int[][] edges = {{0, 1}, {1, 2}, {2, 5}, {0, 3}, {3, 4}, {4, 5}};
        for (int[] e : edges) { g.get(e[0]).add(e[1]); g.get(e[1]).add(e[0]); }
        check(bfs(g, 0, 5) == 3, "algo-bfs");
        check(dfsCount(g, 0) == 6, "algo-dfs-connected");

        List<int[]>[] adj = new List[5];
        for (int i = 0; i < 5; i++) adj[i] = new ArrayList<>();
        adj[0].add(new int[]{1, 4}); adj[0].add(new int[]{2, 1});
        adj[2].add(new int[]{1, 2}); adj[1].add(new int[]{3, 1});
        adj[2].add(new int[]{3, 5}); adj[3].add(new int[]{4, 3});
        int[] dist = dijkstra(5, adj, 0);
        check(dist[0] == 0 && dist[1] == 3 && dist[2] == 1 && dist[3] == 4 && dist[4] == 7, "algo-dijkstra");

        List<Integer>[] dag = new List[6];
        for (int i = 0; i < 6; i++) dag[i] = new ArrayList<>();
        dag[5].add(2); dag[5].add(0); dag[4].add(0); dag[4].add(1); dag[2].add(3); dag[3].add(1);
        List<Integer> order = topo(6, dag);
        check(order.size() == 6, "algo-topo-size");
        int[] pos = new int[6];
        for (int i = 0; i < order.size(); i++) pos[order.get(i)] = i;
        boolean validTopo = true;
        for (int u = 0; u < 6; u++) for (int v : dag[u]) if (pos[u] > pos[v]) validTopo = false;
        check(validTopo, "algo-topo-valid");

        DSU d = new DSU(6);
        check(d.union(0, 1) && d.union(1, 2) && d.union(3, 4), "algo-dsu-union");
        check(!d.union(0, 2), "algo-dsu-already-joined");
        check(d.find(0) == d.find(2) && d.find(0) != d.find(4) && d.find(5) == 5, "algo-dsu-find");

        Trie trie = new Trie();
        trie.insert("apple"); trie.insert("app"); trie.insert("apply");
        check(trie.search("app") && trie.search("apple"), "algo-trie-search");
        check(!trie.search("ap") && trie.startsWith("ap"), "algo-trie-prefix");
        check(!trie.startsWith("xyz"), "algo-trie-noprefix");

        check(kmp("ababcababcabc", "ababc") == 0, "algo-kmp-found");
        check(kmp("aaaaab", "aab") == 3, "algo-kmp-mid");
        check(kmp("abc", "xyz") == -1, "algo-kmp-notfound");

        check(countPrimes(20) == 8, "algo-sieve");
        check(validParen("([]{})") && !validParen("([)]") && !validParen("((("), "algo-valid-parens");

        // PriorityQueue top-k pattern
        PriorityQueue<Integer> pq = new PriorityQueue<>();
        for (int x : new int[]{4, 1, 7, 3, 9, 2}) { pq.add(x); if (pq.size() > 3) pq.poll(); }
        check(pq.peek() == 4, "algo-topk-heap"); // 3 largest are {4,7,9}, min is 4

        LRU<Integer, Integer> lru = new LRU<>(2);
        lru.put(1, 1); lru.put(2, 2); lru.get(1); lru.put(3, 3); // evicts 2 (LRU)
        check(lru.containsKey(1) && lru.containsKey(3) && !lru.containsKey(2), "algo-lru-cache");

        // fibonacci DP
        long[] fib = new long[20]; fib[0] = 0; fib[1] = 1;
        for (int i = 2; i < 20; i++) fib[i] = fib[i - 1] + fib[i - 2];
        check(fib[10] == 55 && fib[19] == 4181, "algo-fibonacci");
    }
}
