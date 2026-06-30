package main

import (
	"errors"
	"fmt"
	"strings"
)

// ---------------------------------------------------------------------------
// CLOSURES + VARIADICS + MULTIPLE/NAMED RETURNS + BLANK IDENTIFIER.
// ---------------------------------------------------------------------------

// makeCounter returns a closure capturing mutable state.
func makeCounter() func() int {
	n := 0
	return func() int { n++; return n }
}

// adder demonstrates variadic + spread.
func adder(xs ...int) int {
	s := 0
	for _, x := range xs {
		s += x
	}
	return s
}

// divmod returns multiple values.
func divmod(a, b int) (int, int) { return a / b, a % b }

// namedReturns uses named result parameters and a naked return.
func namedReturns(a, b int) (sum, diff int) {
	sum = a + b
	diff = a - b
	return // naked
}

// deferModifiesNamed shows defer mutating a named return value.
func deferModifiesNamed() (result int) {
	defer func() { result *= 2 }()
	result = 21
	return // defer runs after, doubling to 42
}

func runClosuresVariadics() {
	section("closures-variadics")

	// Closure with captured state (each closure independent).
	c1 := makeCounter()
	c2 := makeCounter()
	chk("closure/c1-1", c1(), 1)
	chk("closure/c1-2", c1(), 2)
	chk("closure/c1-3", c1(), 3)
	chk("closure/c2-independent", c2(), 1) // separate state

	// Go 1.22+: loop variable is per-iteration, so capturing in closures is safe.
	var fns []func() int
	for i := 0; i < 3; i++ {
		fns = append(fns, func() int { return i })
	}
	got := []int{fns[0](), fns[1](), fns[2]()}
	chkStr("closure/loopvar-1.22", fmt.Sprint(got), "[0 1 2]")

	// Variadic: direct args, zero args, and spread of a slice.
	chk("variadic/args", adder(1, 2, 3, 4), 10)
	chk("variadic/empty", adder(), 0)
	nums := []int{5, 6, 7}
	chk("variadic/spread", adder(nums...), 18)
	chk("variadic/mixed-via-spread", adder(append([]int{1}, nums...)...), 19)

	// Multiple returns.
	q, r := divmod(17, 5)
	chk("multiret/quotient", q, 3)
	chk("multiret/remainder", r, 2)

	// Named returns + naked return.
	s, d := namedReturns(10, 3)
	chk("namedret/sum", s, 13)
	chk("namedret/diff", d, 7)

	// defer mutating a named return.
	chk("namedret/defer-mutate", deferModifiesNamed(), 42)

	// Blank identifier: discard a return value.
	_, onlyRem := divmod(20, 6)
	chk("blank/discard", onlyRem, 2)

	// Blank in range to count only.
	cnt := 0
	for range []int{0, 0, 0, 0} {
		cnt++
	}
	chk("blank/range-count", cnt, 4)

	// Function as first-class value passed around.
	apply := func(f func(int) int, x int) int { return f(x) }
	chk("func/firstclass", apply(func(x int) int { return x * x }, 6), 36)

	// IIFE (immediately-invoked anonymous func).
	chk("func/iife", func(a, b int) int { return a + b }(40, 2), 42)
}

// ---------------------------------------------------------------------------
// DEFER / PANIC / RECOVER (spec: Defer statements, Handling panics).
// ---------------------------------------------------------------------------

// deferOrder records LIFO defer execution into a provided slice pointer.
func deferOrder() string {
	var b strings.Builder
	for i := 1; i <= 3; i++ {
		defer fmt.Fprintf(&b, "%d", i) // captured per-iteration
	}
	// defers run 3,2,1 AFTER this; we return the builder's content via closure.
	// To observe order deterministically, run defers then read — use a wrapper.
	return b.String() // empty here; real read happens in caller wrapper below
}

// deferLIFO returns the order in which defers fire (3,2,1).
func deferLIFO() (order string) {
	defer func() { order += "1" }()
	defer func() { order += "2" }()
	defer func() { order += "3" }()
	return // defers run LIFO: order becomes "321"
}

// deferArgsEvaluatedEarly shows defer captures args at defer time.
func deferArgsEvaluatedEarly() (snapshot int) {
	x := 10
	defer func(captured int) { snapshot = captured }(x) // captures 10 now
	x = 999                                              // later change ignored
	return
}

// recoverPanicVal recovers a panic and returns its value as a string.
func recoverPanicVal() (msg string) {
	defer func() {
		if r := recover(); r != nil {
			msg = fmt.Sprint(r)
		}
	}()
	panic("kaboom")
}

// recoverTyped recovers a panic carrying an error value.
func recoverTyped() (code int) {
	defer func() {
		if r := recover(); r != nil {
			if e, ok := r.(*panicErr); ok {
				code = e.code
			}
		}
	}()
	panic(&panicErr{code: 7})
}

type panicErr struct{ code int }

func (e *panicErr) Error() string { return fmt.Sprintf("panicErr(%d)", e.code) }

// safeDivide returns (result, ok) without propagating a divide-by-zero panic.
func safeDivide(a, b int) (result int, ok bool) {
	defer func() {
		if recover() != nil {
			result, ok = 0, false
		}
	}()
	return a / b, true
}

func runDeferPanicRecover() {
	section("defer-panic-recover")

	_ = deferOrder() // exercise the defer loop path (output observed via deferLIFO)
	chkStr("defer/lifo", deferLIFO(), "321")
	chk("defer/args-early", deferArgsEvaluatedEarly(), 10)

	chkStr("recover/value", recoverPanicVal(), "kaboom")
	chk("recover/typed", recoverTyped(), 7)

	res, ok := safeDivide(20, 4)
	chk("recover/safediv-result", res, 5)
	chkTrue("recover/safediv-ok", ok)
	res2, ok2 := safeDivide(1, 0) // recovers the runtime panic
	chk("recover/safediv-zero-result", res2, 0)
	chk("recover/safediv-zero-ok", ok2, false)

	// Re-panic & recover at outer level.
	chkStr("recover/repanic", func() (s string) {
		defer func() {
			if r := recover(); r != nil {
				s = "outer:" + fmt.Sprint(r)
			}
		}()
		func() {
			defer func() {
				if r := recover(); r != nil {
					panic("re-" + fmt.Sprint(r))
				}
			}()
			panic("boom")
		}()
		return
	}(), "outer:re-boom")
}

// ---------------------------------------------------------------------------
// CONTROL FLOW: switch (expr/no-cond/fallthrough), labels, goto, break/continue.
// ---------------------------------------------------------------------------

func gradeOf(score int) string {
	// Expression-less switch acts like if/else chain.
	switch {
	case score >= 90:
		return "A"
	case score >= 80:
		return "B"
	case score >= 70:
		return "C"
	default:
		return "F"
	}
}

func runControlFlow() {
	section("control-flow")

	// Expression switch with multiple case values.
	day := 6
	var kind string
	switch day {
	case 0, 6:
		kind = "weekend"
	case 1, 2, 3, 4, 5:
		kind = "weekday"
	}
	chkStr("switch/multi-value", kind, "weekend")

	// switch with init statement.
	switch n := day * 2; {
	case n > 10:
		chk("switch/init", n, 12)
	default:
		chk("switch/init", 0, 1)
	}

	// fallthrough explicit.
	var ft string
	switch 1 {
	case 1:
		ft += "1"
		fallthrough
	case 2:
		ft += "2"
		fallthrough
	case 3:
		ft += "3"
	case 4:
		ft += "4" // not reached
	}
	chkStr("switch/fallthrough", ft, "123")

	// Expression-less switch as grade ladder.
	chkStr("switch/grade-A", gradeOf(95), "A")
	chkStr("switch/grade-C", gradeOf(72), "C")
	chkStr("switch/grade-F", gradeOf(50), "F")

	// Labeled break out of nested loops.
	found := -1
outer:
	for i := 0; i < 5; i++ {
		for j := 0; j < 5; j++ {
			if i*5+j == 13 {
				found = i*10 + j
				break outer
			}
		}
	}
	chk("label/break", found, 23) // i=2,j=3

	// Labeled continue.
	var collected []int
loop:
	for i := 0; i < 3; i++ {
		for j := 0; j < 3; j++ {
			if j == 1 {
				continue loop // skip rest of inner, advance outer
			}
			collected = append(collected, i*10+j)
		}
	}
	chkStr("label/continue", fmt.Sprint(collected), "[0 10 20]")

	// goto for a simple loop.
	sum, i := 0, 1
loopGoto:
	if i <= 5 {
		sum += i
		i++
		goto loopGoto
	}
	chk("goto/loop", sum, 15)
}

// ---------------------------------------------------------------------------
// ERROR HANDLING (language + errors pkg: Is/As/Join/Unwrap/%w).
// ---------------------------------------------------------------------------

// sentinel errors.
var (
	errNotFound = errors.New("not found")
	errDenied   = errors.New("denied")
)

// codeError is a custom error carrying a code, for errors.As.
type codeError struct {
	code int
	msg  string
}

func (e *codeError) Error() string { return fmt.Sprintf("[%d] %s", e.code, e.msg) }

// fetch wraps a sentinel with %w to build a chain.
func fetch(found bool) error {
	if !found {
		return fmt.Errorf("fetch failed: %w", errNotFound)
	}
	return nil
}

func runErrorsLanguage() {
	section("errors-language")

	// errors.New + Error().
	e := errors.New("boom")
	chkStr("err/new", e.Error(), "boom")

	// %w wrapping + errors.Is finds the sentinel deep in the chain.
	werr := fetch(false)
	chkTrue("err/is-sentinel", errors.Is(werr, errNotFound))
	chk("err/is-other", errors.Is(werr, errDenied), false)
	chkStr("err/wrapped-message", werr.Error(), "fetch failed: not found")

	// errors.Unwrap peels one layer.
	chkTrue("err/unwrap", errors.Unwrap(werr) == errNotFound)

	// errors.As extracts a concrete typed error from the chain.
	chain := fmt.Errorf("ctx: %w", &codeError{code: 42, msg: "bad"})
	var ce *codeError
	chkTrue("err/as-ok", errors.As(chain, &ce))
	chk("err/as-code", ce.code, 42)
	chkStr("err/as-msg", ce.msg, "bad")

	// errors.As fails for an unrelated type.
	var other *panicErr
	chk("err/as-miss", errors.As(chain, &other), false)

	// errors.Join combines multiple errors; Is finds each.
	joined := errors.Join(errNotFound, errDenied)
	chkTrue("err/join-is-1", errors.Is(joined, errNotFound))
	chkTrue("err/join-is-2", errors.Is(joined, errDenied))
	chkStr("err/join-message", joined.Error(), "not found\ndenied")

	// Join with a nil drops it (Join ignores nils).
	j2 := errors.Join(nil, errNotFound, nil)
	chkStr("err/join-nil-skip", j2.Error(), "not found")
	chkTrue("err/join-all-nil", errors.Join(nil, nil) == nil)

	// Double-wrap chain still found.
	deep := fmt.Errorf("layer3: %w", fmt.Errorf("layer2: %w", errDenied))
	chkTrue("err/deep-is", errors.Is(deep, errDenied))

	// Comparing nil error.
	chkTrue("err/nil", fetch(true) == nil)
}
