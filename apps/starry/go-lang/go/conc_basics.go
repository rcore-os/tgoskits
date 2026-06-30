package main

import (
	"context"
	"errors"
	"fmt"
	"sort"
	"sync"
	"time"
)

// ---------------------------------------------------------------------------
// GOROUTINES + CHANNELS (spec: Go statements, Channel types, Send/Receive).
// All outputs are scheduling-independent: sums, sorted dumps, fixed counts.
// ---------------------------------------------------------------------------

func runGoroutinesChannels() {
	section("goroutines-channels")

	// Fan many goroutines into a buffered channel; sum is order-independent.
	const N = 100
	ch := make(chan int, N)
	var wg sync.WaitGroup
	for i := 1; i <= N; i++ {
		wg.Add(1)
		go func(v int) {
			defer wg.Done()
			ch <- v
		}(i)
	}
	wg.Wait()
	close(ch)
	sum := 0
	for v := range ch {
		sum += v
	}
	chk("chan/buffered-sum", sum, 5050)

	// Unbuffered channel: synchronous handoff between two goroutines.
	done := make(chan int)
	go func() { done <- 42 }()
	chk("chan/unbuffered", <-done, 42)

	// Directional channel types (send-only / receive-only).
	produce := func(out chan<- int) {
		for i := 1; i <= 5; i++ {
			out <- i
		}
		close(out)
	}
	consume := func(in <-chan int) int {
		s := 0
		for v := range in {
			s += v
		}
		return s
	}
	pipe := make(chan int, 5)
	go produce(pipe)
	chk("chan/directional", consume(pipe), 15)

	// Closed channel: receive yields zero value + ok=false.
	cc := make(chan int, 1)
	cc <- 9
	close(cc)
	v1, ok1 := <-cc
	chk("chan/closed-drain-value", v1, 9)
	chkTrue("chan/closed-drain-ok", ok1)
	v2, ok2 := <-cc
	chk("chan/closed-empty-value", v2, 0)
	chk("chan/closed-empty-ok", ok2, false)

	// Buffered channel cap & len.
	bc := make(chan int, 4)
	bc <- 1
	bc <- 2
	chk("chan/cap", cap(bc), 4)
	chk("chan/len", len(bc), 2)

	// Worker collecting results into a map, read back via sorted keys.
	results := make(chan struct {
		k string
		v int
	}, 3)
	go func() {
		results <- struct {
			k string
			v int
		}{"alpha", 1}
		results <- struct {
			k string
			v int
		}{"beta", 2}
		results <- struct {
			k string
			v int
		}{"gamma", 3}
		close(results)
	}()
	collected := map[string]int{}
	for r := range results {
		collected[r.k] = r.v
	}
	keys := make([]string, 0, len(collected))
	for k := range collected {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	var b []string
	for _, k := range keys {
		b = append(b, fmt.Sprintf("%s=%d", k, collected[k]))
	}
	chkStr("chan/collected-sorted", fmt.Sprint(b), "[alpha=1 beta=2 gamma=3]")
}

// ---------------------------------------------------------------------------
// SELECT (spec: Select statements) — default, timeout, send/recv readiness.
// ---------------------------------------------------------------------------

func runSelect() {
	section("select")

	// select with default: non-blocking receive on empty channel.
	empty := make(chan int)
	var got string
	select {
	case <-empty:
		got = "recv"
	default:
		got = "default"
	}
	chkStr("select/default", got, "default")

	// select picks the ready channel.
	ready := make(chan int, 1)
	ready <- 7
	var picked int
	select {
	case picked = <-ready:
	default:
		picked = -1
	}
	chk("select/ready", picked, 7)

	// select with timeout: idle channel never sends, timer wins.
	idle := make(chan int)
	var outcome string
	select {
	case <-idle:
		outcome = "chan"
	case <-time.After(20 * time.Millisecond):
		outcome = "timeout"
	}
	chkStr("select/timeout", outcome, "timeout")

	// select non-blocking send: buffer has room -> send succeeds.
	buf := make(chan int, 1)
	var sent bool
	select {
	case buf <- 1:
		sent = true
	default:
		sent = false
	}
	chkTrue("select/send-ok", sent)
	// Now full -> default taken.
	select {
	case buf <- 2:
		sent = true
	default:
		sent = false
	}
	chk("select/send-full", sent, false)

	// Loop draining two channels via select until both closed.
	a := make(chan int, 2)
	bch := make(chan int, 2)
	a <- 1
	a <- 2
	bch <- 10
	bch <- 20
	close(a)
	close(bch)
	total, openA, openB := 0, true, true
	for openA || openB {
		select {
		case v, ok := <-a:
			if !ok {
				a = nil // disable this case
				openA = false
				continue
			}
			total += v
		case v, ok := <-bch:
			if !ok {
				bch = nil
				openB = false
				continue
			}
			total += v
		}
	}
	chk("select/drain-two", total, 33)
}

// ---------------------------------------------------------------------------
// CONTEXT (pkg context: cancel, deadline/timeout, value, cause).
// ---------------------------------------------------------------------------

type ctxKey string

func runContext() {
	section("context")

	// WithCancel: cancel propagates to ctx.Err().
	ctx, cancel := context.WithCancel(context.Background())
	chkTrue("ctx/before-cancel", ctx.Err() == nil)
	cancel()
	chkTrue("ctx/canceled", errors.Is(ctx.Err(), context.Canceled))

	// Done channel is closed after cancel (non-blocking receive succeeds).
	select {
	case <-ctx.Done():
		chkTrue("ctx/done-closed", true)
	default:
		chk("ctx/done-closed", 0, 1)
	}

	// WithTimeout: short timeout elapses -> DeadlineExceeded.
	tctx, tcancel := context.WithTimeout(context.Background(), 10*time.Millisecond)
	defer tcancel()
	<-tctx.Done()
	chkTrue("ctx/deadline-exceeded", errors.Is(tctx.Err(), context.DeadlineExceeded))

	// WithValue: retrieve a typed value.
	vctx := context.WithValue(context.Background(), ctxKey("user"), "leo")
	chkStr("ctx/value", vctx.Value(ctxKey("user")).(string), "leo")
	chkTrue("ctx/value-missing", vctx.Value(ctxKey("nope")) == nil)

	// WithCancelCause: cancel with a custom cause.
	cc, ccancel := context.WithCancelCause(context.Background())
	myCause := errors.New("user aborted")
	ccancel(myCause)
	chkTrue("ctx/cause", errors.Is(context.Cause(cc), myCause))

	// A goroutine observes cancellation deterministically (waits on Done).
	pctx, pcancel := context.WithCancel(context.Background())
	observed := make(chan error, 1)
	go func() {
		<-pctx.Done()
		observed <- pctx.Err()
	}()
	pcancel()
	chkTrue("ctx/goroutine-observes", errors.Is(<-observed, context.Canceled))
}
