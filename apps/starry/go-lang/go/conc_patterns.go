package main

import (
	"sort"
	"sync"
)

// ---------------------------------------------------------------------------
// CLASSIC CONCURRENCY PATTERNS — worker pool, fan-out/fan-in, pipeline.
// Outputs are scheduling-independent: we sum results or sort them before
// asserting, so the golden file never depends on goroutine ordering.
// ---------------------------------------------------------------------------

// workerPool dispatches jobs 1..n to W workers that square them; returns the
// sum of squares (order-independent) and the count of results.
func workerPool(n, w int) (sumSquares, count int) {
	jobs := make(chan int, n)
	results := make(chan int, n)
	var wg sync.WaitGroup
	for k := 0; k < w; k++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for j := range jobs {
				results <- j * j
			}
		}()
	}
	for j := 1; j <= n; j++ {
		jobs <- j
	}
	close(jobs)
	go func() { wg.Wait(); close(results) }()
	for r := range results {
		sumSquares += r
		count++
	}
	return
}

// fanOutFanIn fans a single input stream out to W workers (each doubles the
// value) then fans results back into one channel; returns the order-independent
// sum and the sorted multiset of results as a slice.
func fanOutFanIn(inputs []int, w int) (sum int, sorted []int) {
	in := make(chan int)
	out := make(chan int)
	go func() {
		for _, v := range inputs {
			in <- v
		}
		close(in)
	}()
	var wg sync.WaitGroup
	for k := 0; k < w; k++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for v := range in {
				out <- v * 2
			}
		}()
	}
	go func() { wg.Wait(); close(out) }()
	for r := range out {
		sum += r
		sorted = append(sorted, r)
	}
	sort.Ints(sorted)
	return
}

// pipeline: generate -> square -> filter-even. Each stage is a goroutine
// connected by channels. Returns the order-preserving result (single path so
// order is deterministic) as a sum + slice.
func pipeline(n int) (sum int, vals []int) {
	gen := func() <-chan int {
		ch := make(chan int)
		go func() {
			defer close(ch)
			for i := 1; i <= n; i++ {
				ch <- i
			}
		}()
		return ch
	}
	square := func(in <-chan int) <-chan int {
		ch := make(chan int)
		go func() {
			defer close(ch)
			for v := range in {
				ch <- v * v
			}
		}()
		return ch
	}
	evenOnly := func(in <-chan int) <-chan int {
		ch := make(chan int)
		go func() {
			defer close(ch)
			for v := range in {
				if v%2 == 0 {
					ch <- v
				}
			}
		}()
		return ch
	}
	// A single linear pipeline preserves order, so vals is deterministic.
	for v := range evenOnly(square(gen())) {
		sum += v
		vals = append(vals, v)
	}
	return
}

func runConcurrencyPatterns() {
	section("concurrency-patterns")

	// Worker pool: sum of squares 1..10 = 385, with 4 workers.
	ss, cnt := workerPool(10, 4)
	chk("pattern/workerpool-sumsq", ss, 385)
	chk("pattern/workerpool-count", cnt, 10)
	// Independent of worker count.
	ss2, _ := workerPool(10, 1)
	chk("pattern/workerpool-1worker", ss2, 385)
	ss3, _ := workerPool(10, 16)
	chk("pattern/workerpool-16workers", ss3, 385)

	// Fan-out/fan-in: doubling [1..5] -> sum 30, sorted [2 4 6 8 10].
	sum, sorted := fanOutFanIn([]int{1, 2, 3, 4, 5}, 3)
	chk("pattern/faninout-sum", sum, 30)
	chk("pattern/faninout-len", len(sorted), 5)
	chk("pattern/faninout-min", sorted[0], 2)
	chk("pattern/faninout-max", sorted[4], 10)

	// Pipeline: 1..6 -> squares -> even -> [4 16 36], sum 56.
	psum, pvals := pipeline(6)
	chk("pattern/pipeline-sum", psum, 56)
	chkStr("pattern/pipeline-vals", intsToStr(pvals), "[4 16 36]")
}
