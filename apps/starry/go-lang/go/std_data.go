package main

import (
	"cmp"
	"fmt"
	"maps"
	"math"
	"math/big"
	"math/bits"
	mrand "math/rand/v2"
	"regexp"
	"slices"
	"sort"
	"strings"
	"time"
)

// ---------------------------------------------------------------------------
// sort + slices + maps + cmp.
// ---------------------------------------------------------------------------

func runStdSortSlicesMapsCmp() {
	section("std-sort-slices-maps-cmp")

	// sort (legacy API).
	xs := []int{5, 2, 8, 1, 9}
	sort.Ints(xs)
	chkStr("sort/Ints", fmt.Sprint(xs), "[1 2 5 8 9]")
	ss := []string{"banana", "apple", "cherry"}
	sort.Strings(ss)
	chkStr("sort/Strings", fmt.Sprint(ss), "[apple banana cherry]")
	fs := []float64{3.3, 1.1, 2.2}
	sort.Float64s(fs)
	chkStr("sort/Float64s", fmt.Sprint(fs), "[1.1 2.2 3.3]")
	chkTrue("sort/IntsAreSorted", sort.IntsAreSorted(xs))
	chk("sort/SearchInts", sort.SearchInts(xs, 8), 3)
	// sort.Slice with a custom less.
	people := []struct {
		name string
		age  int
	}{{"c", 30}, {"a", 20}, {"b", 25}}
	sort.Slice(people, func(i, j int) bool { return people[i].age < people[j].age })
	chkStr("sort/Slice", people[0].name, "a")
	chkTrue("sort/SliceIsSorted", sort.SliceIsSorted(people, func(i, j int) bool { return people[i].age < people[j].age }))

	// slices package (Go 1.21+).
	sl := []int{3, 1, 2}
	slices.Sort(sl)
	chkStr("slices/Sort", fmt.Sprint(sl), "[1 2 3]")
	chk("slices/Contains", boolToInt(slices.Contains(sl, 2)), 1)
	chk("slices/Index", slices.Index(sl, 3), 2)
	chk("slices/Max", slices.Max(sl), 3)
	chk("slices/Min", slices.Min(sl), 1)
	chk("slices/Equal", boolToInt(slices.Equal([]int{1, 2}, []int{1, 2})), 1)
	chkStr("slices/Reverse", reversedStr([]int{1, 2, 3}), "[3 2 1]")
	chkStr("slices/Insert", fmt.Sprint(slices.Insert([]int{1, 4}, 1, 2, 3)), "[1 2 3 4]")
	chkStr("slices/Delete", fmt.Sprint(slices.Delete([]int{1, 2, 3, 4}, 1, 3)), "[1 4]")
	chkStr("slices/Compact", fmt.Sprint(slices.Compact([]int{1, 1, 2, 2, 3})), "[1 2 3]")
	chkStr("slices/Clone", fmt.Sprint(slices.Clone([]int{1, 2})), "[1 2]")
	chk("slices/BinarySearch", binSearchIdx([]int{1, 3, 5, 7}, 5), 2)
	chk("slices/IndexFunc", slices.IndexFunc([]int{1, 2, 3}, func(x int) bool { return x > 1 }), 1)
	chk("slices/ContainsFunc", boolToInt(slices.ContainsFunc([]int{1, 2}, func(x int) bool { return x == 2 })), 1)
	chkStr("slices/SortFunc-desc", sortDescStr([]int{1, 3, 2}), "[3 2 1]")
	chk("slices/MaxFunc", slices.MaxFunc([]int{1, -5, 3}, func(a, b int) int { return cmp.Compare(abs(a), abs(b)) }), -5)
	// slices.Collect from an iterator (Go 1.23).
	collected := slices.Collect(countUp(4))
	chkStr("slices/Collect", fmt.Sprint(collected), "[1 2 3 4]")
	chkStr("slices/Sorted", fmt.Sprint(slices.Sorted(slices.Values([]int{3, 1, 2}))), "[1 2 3]")
	chkStr("slices/Concat", fmt.Sprint(slices.Concat([]int{1}, []int{2, 3})), "[1 2 3]")

	// maps package (Go 1.21+): Keys/Values yield iterators -> sort to assert.
	m := map[string]int{"b": 2, "a": 1, "c": 3}
	mk := slices.Sorted(maps.Keys(m))
	chkStr("maps/Keys-sorted", strings.Join(mk, ""), "abc")
	mv := slices.Sorted(maps.Values(m))
	chkStr("maps/Values-sorted", fmt.Sprint(mv), "[1 2 3]")
	cloned := maps.Clone(m)
	chk("maps/Clone-len", len(cloned), 3)
	chkTrue("maps/Equal", maps.Equal(m, cloned))
	dst := map[string]int{"x": 9}
	maps.Copy(dst, map[string]int{"y": 10})
	chk("maps/Copy-len", len(dst), 2)
	maps.DeleteFunc(m, func(k string, v int) bool { return v == 2 })
	chk("maps/DeleteFunc", len(m), 2)

	// cmp package (Go 1.21+).
	chk("cmp/Compare-lt", cmp.Compare(1, 2), -1)
	chk("cmp/Compare-eq", cmp.Compare(2, 2), 0)
	chk("cmp/Compare-gt", cmp.Compare(3, 2), 1)
	chk("cmp/Less", boolToInt(cmp.Less(1, 2)), 1)
	chk("cmp/Or", cmp.Or(0, 0, 5, 7), 5)        // first non-zero
	chkStr("cmp/Or-string", cmp.Or("", "x", "y"), "x")
}

func reversedStr(xs []int) string {
	c := slices.Clone(xs)
	slices.Reverse(c)
	return fmt.Sprint(c)
}
func binSearchIdx(xs []int, v int) int { i, _ := slices.BinarySearch(xs, v); return i }
func sortDescStr(xs []int) string {
	c := slices.Clone(xs)
	slices.SortFunc(c, func(a, b int) int { return cmp.Compare(b, a) })
	return fmt.Sprint(c)
}
func abs(x int) int {
	if x < 0 {
		return -x
	}
	return x
}

// ---------------------------------------------------------------------------
// math + math/big + math/bits + math/rand/v2 (seeded, deterministic).
// ---------------------------------------------------------------------------

func runStdMath() {
	section("std-math")

	// math constants & functions.
	piScaled := math.Pi * 100
	chk("math/Pi-trunc", int(piScaled), 314)
	chk("math/Sqrt", math.Sqrt(144), 12.0)
	chk("math/Pow", math.Pow(2, 10), 1024.0)
	chk("math/Abs", math.Abs(-3.5), 3.5)
	chk("math/Floor", math.Floor(3.7), 3.0)
	chk("math/Ceil", math.Ceil(3.2), 4.0)
	chk("math/Round", math.Round(2.5), 3.0)
	chk("math/Trunc", math.Trunc(3.9), 3.0)
	chk("math/Max", math.Max(3, 7), 7.0)
	chk("math/Min", math.Min(3, 7), 3.0)
	chk("math/Mod", math.Mod(10, 3), 1.0)
	chk("math/Hypot", math.Hypot(3, 4), 5.0)
	chk("math/MaxInt32", math.MaxInt32, 2147483647)
	chk("math/Log2", math.Log2(8), 3.0)
	chk("math/Log10", math.Log10(1000), 3.0)
	chk("math/Cbrt", math.Cbrt(27), 3.0)
	chkTrue("math/IsNaN", math.IsNaN(math.NaN()))
	chkTrue("math/IsInf", math.IsInf(math.Inf(1), 1))
	chk("math/Signbit", boolToInt(math.Signbit(-1)), 1)
	chk("math/Copysign", math.Copysign(3, -1), -3.0)
	gcdHelper := func(a, b int) int {
		for b != 0 {
			a, b = b, a%b
		}
		return a
	}
	chk("math/gcd-helper", gcdHelper(48, 36), 12)

	// math/bits.
	chk("bits/OnesCount", bits.OnesCount(0b1011), 3)
	chk("bits/LeadingZeros8", bits.LeadingZeros8(1), 7)
	chk("bits/TrailingZeros", bits.TrailingZeros(8), 3)
	chk("bits/Len", bits.Len(255), 8)
	chk("bits/Reverse8", int(bits.Reverse8(1)), 128)
	chk("bits/RotateLeft8", int(bits.RotateLeft8(1, 1)), 2)
	chk("bits/UintSize", bits.UintSize, 64) // on a 64-bit build

	// math/big: arbitrary precision factorial 20!.
	fact := big.NewInt(1)
	for i := int64(2); i <= 20; i++ {
		fact.Mul(fact, big.NewInt(i))
	}
	chkStr("big/factorial20", fact.String(), "2432902008176640000")
	// big beyond int64 (25!) to prove arbitrary precision.
	for i := int64(21); i <= 25; i++ {
		fact.Mul(fact, big.NewInt(i))
	}
	chkStr("big/factorial25", fact.String(), "15511210043330985984000000")
	// big.Rat exact fraction.
	r := big.NewRat(1, 3)
	r.Add(r, big.NewRat(1, 6))
	chkStr("big/rat", r.String(), "1/2")
	// big.Int from string + Cmp.
	a, _ := new(big.Int).SetString("123456789012345678901234567890", 10)
	chk("big/cmp", a.Cmp(big.NewInt(0)), 1)

	// math/rand/v2 with a fixed seed: deterministic sequence.
	rng := mrand.New(mrand.NewPCG(42, 42))
	first := rng.IntN(1000)
	second := rng.IntN(1000)
	// Re-seed identically: same sequence.
	rng2 := mrand.New(mrand.NewPCG(42, 42))
	chk("rand/deterministic-1", rng2.IntN(1000), first)
	chk("rand/deterministic-2", rng2.IntN(1000), second)
	// Shuffle with a fixed source is reproducible.
	deck := []int{1, 2, 3, 4, 5}
	rng3 := mrand.New(mrand.NewPCG(7, 7))
	rng3.Shuffle(len(deck), func(i, j int) { deck[i], deck[j] = deck[j], deck[i] })
	// The permutation is fixed; assert it is a permutation (sum invariant) +
	// reproducibility against a second identical run.
	deck2 := []int{1, 2, 3, 4, 5}
	rng4 := mrand.New(mrand.NewPCG(7, 7))
	rng4.Shuffle(len(deck2), func(i, j int) { deck2[i], deck2[j] = deck2[j], deck2[i] })
	chkStr("rand/shuffle-reproducible", fmt.Sprint(deck), fmt.Sprint(deck2))
	chk("rand/shuffle-permutation-sum", sumInts(deck), 15)
}

func sumInts(xs []int) int {
	s := 0
	for _, x := range xs {
		s += x
	}
	return s
}

// ---------------------------------------------------------------------------
// time (fixed instants only — never the wall clock).
// ---------------------------------------------------------------------------

func runStdTime() {
	section("std-time")

	t := time.Date(2026, time.May, 24, 12, 34, 56, 0, time.UTC)
	chkStr("time/Format-RFC3339", t.Format(time.RFC3339), "2026-05-24T12:34:56Z")
	chkStr("time/Format-custom", t.Format("2006/01/02 15:04:05"), "2026/05/24 12:34:56")
	chk("time/Year", t.Year(), 2026)
	chkStr("time/Month", t.Month().String(), "May")
	chk("time/Day", t.Day(), 24)
	chk("time/Hour", t.Hour(), 12)
	chkStr("time/Weekday", t.Weekday().String(), "Sunday")
	chk("time/Unix", int(t.Unix()), 1779626096) // 2026-05-24T12:34:56Z

	// Parse round-trips.
	parsed, _ := time.Parse(time.RFC3339, "2026-05-24T12:34:56Z")
	chkTrue("time/Parse-equal", parsed.Equal(t))

	// Arithmetic with durations.
	later := t.Add(48 * time.Hour)
	chk("time/Add-day", later.Day(), 26)
	diff := later.Sub(t)
	chk("time/Sub-hours", int(diff.Hours()), 48)
	chkTrue("time/After", later.After(t))
	chkTrue("time/Before", t.Before(later))

	// AddDate.
	nextMonth := t.AddDate(0, 1, 0)
	chkStr("time/AddDate-month", nextMonth.Month().String(), "June")

	// Duration parsing & formatting.
	d, _ := time.ParseDuration("1h30m")
	chk("time/Duration-minutes", int(d.Minutes()), 90)
	chkStr("time/Duration-string", (90 * time.Minute).String(), "1h30m0s")
	chk("time/Duration-seconds", int((2 * time.Minute).Seconds()), 120)

	// Truncate / Round on a fixed instant.
	chk("time/Truncate-hour", t.Truncate(time.Hour).Minute(), 0)

	// Comparison helper.
	chk("time/Compare", t.Compare(later), -1)
}

// ---------------------------------------------------------------------------
// regexp.
// ---------------------------------------------------------------------------

func runStdRegexp() {
	section("std-regexp")

	re := regexp.MustCompile(`\d+`)
	chkTrue("regexp/MatchString", re.MatchString("abc123"))
	chkStr("regexp/FindString", re.FindString("abc123def456"), "123")
	chkStr("regexp/FindAllString", fmt.Sprint(re.FindAllString("a1b22c333", -1)), "[1 22 333]")
	chkStr("regexp/ReplaceAllString", re.ReplaceAllString("a1b2", "#"), "a#b#")

	// Submatches / named groups.
	kv := regexp.MustCompile(`(?P<key>\w+)=(?P<val>\w+)`)
	m := kv.FindStringSubmatch("name=leo")
	chkStr("regexp/Submatch-full", m[0], "name=leo")
	chkStr("regexp/Submatch-key", m[1], "name")
	chkStr("regexp/Submatch-val", m[2], "leo")
	idx := kv.SubexpIndex("val")
	chkStr("regexp/SubexpIndex", m[idx], "leo")

	// Anchors / alternation / char classes.
	chkTrue("regexp/anchored", regexp.MustCompile(`^go$`).MatchString("go"))
	chk("regexp/anchored-false", boolToInt(regexp.MustCompile(`^go$`).MatchString("going")), 0)
	chkTrue("regexp/alternation", regexp.MustCompile(`cat|dog`).MatchString("hotdog"))
	chkStr("regexp/Split", fmt.Sprint(regexp.MustCompile(`\s+`).Split("a  b   c", -1)), "[a b c]")
	chk("regexp/NumSubexp", regexp.MustCompile(`(a)(b)(c)`).NumSubexp(), 3)

	// ReplaceAllStringFunc.
	upper := regexp.MustCompile(`[a-z]+`).ReplaceAllStringFunc("hi there", strings.ToUpper)
	chkStr("regexp/ReplaceFunc", upper, "HI THERE")

	// QuoteMeta escapes regex metacharacters.
	chkStr("regexp/QuoteMeta", regexp.QuoteMeta("a.b*c"), `a\.b\*c`)
}
