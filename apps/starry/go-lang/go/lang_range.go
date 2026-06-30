package main

import (
	"fmt"
	"iter"
	"strings"
)

// ---------------------------------------------------------------------------
// RANGE FORMS (spec: For statements with range clause).
//   - range over slice/array/string/map/channel
//   - range over integer (Go 1.22)
//   - range over function — iterators Seq / Seq2 (Go 1.23, pkg iter)
// ---------------------------------------------------------------------------

// countUp is an iter.Seq[int] yielding 1..n.
func countUp(n int) iter.Seq[int] {
	return func(yield func(int) bool) {
		for i := 1; i <= n; i++ {
			if !yield(i) {
				return
			}
		}
	}
}

// enumerate is an iter.Seq2[int, string] yielding (index, value) pairs.
func enumerate(xs []string) iter.Seq2[int, string] {
	return func(yield func(int, string) bool) {
		for i, v := range xs {
			if !yield(i, v) {
				return
			}
		}
	}
}

// pullDemo uses iter.Pull to drive a Seq imperatively.
func pullSum(seq iter.Seq[int]) int {
	next, stop := iter.Pull(seq)
	defer stop()
	sum := 0
	for {
		v, ok := next()
		if !ok {
			break
		}
		sum += v
	}
	return sum
}

func runRangeForms() {
	section("range-forms")

	// range over slice: index + value.
	sl := []int{10, 20, 30}
	idxSum, valSum := 0, 0
	for i, v := range sl {
		idxSum += i
		valSum += v
	}
	chk("range/slice-idxsum", idxSum, 3) // 0+1+2
	chk("range/slice-valsum", valSum, 60)

	// range over array.
	arr := [4]int{1, 2, 3, 4}
	asum := 0
	for _, v := range arr {
		asum += v
	}
	chk("range/array-sum", asum, 10)

	// range over string: byte index + rune.
	rsum := 0
	for _, r := range "abc" {
		rsum += int(r)
	}
	chk("range/string-runesum", rsum, int('a'+'b'+'c'))

	// range over map: value sum (order-independent).
	m := map[string]int{"a": 1, "b": 2, "c": 3}
	msum := 0
	for _, v := range m {
		msum += v
	}
	chk("range/map-valsum", msum, 6)

	// range over channel (closed): collect order-independent sum.
	ch := make(chan int, 3)
	ch <- 5
	ch <- 6
	ch <- 7
	close(ch)
	csum := 0
	for v := range ch {
		csum += v
	}
	chk("range/channel-sum", csum, 18)

	// range over integer (Go 1.22): for i := range N.
	intSum := 0
	for i := range 5 { // i = 0,1,2,3,4
		intSum += i
	}
	chk("range/int-sum", intSum, 10)
	// range over integer with no variable.
	loops := 0
	for range 4 {
		loops++
	}
	chk("range/int-novar", loops, 4)

	// range over function — iter.Seq[int] (Go 1.23).
	seqSum := 0
	for v := range countUp(5) {
		seqSum += v
	}
	chk("range/seq-sum", seqSum, 15)

	// Early break stops the iterator (yield returns false).
	collected := []int{}
	for v := range countUp(100) {
		if v > 3 {
			break
		}
		collected = append(collected, v)
	}
	chkStr("range/seq-break", fmt.Sprint(collected), "[1 2 3]")

	// range over function — iter.Seq2[int,string] (Go 1.23).
	var pairs []string
	for i, v := range enumerate([]string{"x", "y", "z"}) {
		pairs = append(pairs, fmt.Sprintf("%d:%s", i, v))
	}
	chkStr("range/seq2", strings.Join(pairs, ","), "0:x,1:y,2:z")

	// iter.Pull: pull-style iteration.
	chk("range/iter-pull", pullSum(countUp(10)), 55)
}
