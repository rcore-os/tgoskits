package main

import (
	"fmt"
	"sort"
	"strings"
	"unsafe"
)

// unsafeSizeofEmpty returns the size of an empty struct (0). Demonstrates the
// unsafe package without any pointer arithmetic that would be non-deterministic.
func unsafeSizeofEmpty() uintptr { return unsafe.Sizeof(struct{}{}) }

// ---------------------------------------------------------------------------
// METHODS + INTERFACES (spec: Method sets, Interface types, type sets).
// ---------------------------------------------------------------------------

// counter has both value and pointer receiver methods to exercise method sets.
type counter struct{ n int }

func (c counter) Get() int   { return c.n }    // value receiver: read
func (c *counter) Inc()      { c.n++ }         // pointer receiver: mutate
func (c *counter) Add(d int) { c.n += d }      // pointer receiver

// shape is a basic (method-only) interface.
type shape interface {
	Area() float64
	Perimeter() float64
}

type rect struct{ w, h float64 }

func (r rect) Area() float64      { return r.w * r.h }
func (r rect) Perimeter() float64 { return 2 * (r.w + r.h) }

type square struct{ s float64 }

func (q square) Area() float64      { return q.s * q.s }
func (q square) Perimeter() float64 { return 4 * q.s }

// namer is a one-method interface for embedding.
type namer interface{ Name() string }

// describer embeds namer (interface embedding) and adds a method.
type describer interface {
	namer
	Kind() string
}

type widget struct{ id int }

func (w widget) Name() string   { return fmt.Sprintf("w%d", w.id) }
func (w widget) Kind() string   { return "widget" }
func (w widget) String() string { return fmt.Sprintf("w%d", w.id) } // fmt.Stringer

func runMethodsInterfaces() {
	section("methods-interfaces")

	// Value vs pointer receiver.
	c := counter{n: 5}
	chk("method/value-recv", c.Get(), 5)
	c.Inc() // addressable -> auto &c
	chk("method/ptr-recv-inc", c.n, 6)
	c.Add(4)
	chk("method/ptr-recv-add", c.n, 10)

	// Method value & method expression.
	getter := c.Get             // method value bound to c (snapshot of c at bind)
	chk("method/value-bound", getter(), 10)
	expr := counter.Get         // method expression: takes receiver explicitly
	chk("method/expression", expr(counter{n: 77}), 77)

	// Interface satisfaction + dynamic dispatch.
	var sh shape = rect{w: 3, h: 4}
	chk("iface/dispatch-area", sh.Area(), 12.0)
	chk("iface/dispatch-perim", sh.Perimeter(), 14.0)
	sh = square{s: 5}
	chk("iface/reassign-area", sh.Area(), 25.0)

	// Polymorphism over a slice of the interface; deterministic order.
	shapes := []shape{rect{2, 3}, square{4}, rect{1, 1}}
	var areaSum float64
	for _, x := range shapes {
		areaSum += x.Area()
	}
	chk("iface/poly-areasum", areaSum, 6.0+16.0+1.0)

	// Interface embedding: a widget satisfies describer (and namer).
	var d describer = widget{id: 3}
	chkStr("iface/embed-name", d.Name(), "w3")
	chkStr("iface/embed-kind", d.Kind(), "widget")
	var nm namer = d // describer is assignable to namer
	chkStr("iface/embed-upcast", nm.Name(), "w3")

	// nil interface vs typed-nil.
	var emptyIface interface{}
	chkTrue("iface/nil", emptyIface == nil)

	// fmt.Stringer is satisfied implicitly; %v uses it.
	chkStr("iface/stringer", fmt.Sprint(widget{id: 9}), "w9")

	// any (alias for interface{}) holding different dynamic types.
	vals := []any{1, "two", 3.0, true}
	chk("iface/any-count", len(vals), 4)
}

// ---------------------------------------------------------------------------
// TYPE ASSERTIONS + TYPE SWITCH (spec: Type assertions, Type switches).
// ---------------------------------------------------------------------------

func classify(v any) string {
	switch x := v.(type) {
	case nil:
		return "nil"
	case int:
		return fmt.Sprintf("int:%d", x)
	case string:
		return fmt.Sprintf("string:%d", len(x))
	case bool:
		return fmt.Sprintf("bool:%v", x)
	case []int:
		s := 0
		for _, e := range x {
			s += e
		}
		return fmt.Sprintf("[]int:%d", s)
	case fmt.Stringer:
		return "stringer:" + x.String()
	default:
		return fmt.Sprintf("other:%T", x)
	}
}

func runTypeAssertSwitch() {
	section("type-assert-switch")

	// Single-value assertion (panics on failure) — used on a known-good type.
	var i any = 42
	n := i.(int)
	chk("assert/single", n, 42)

	// comma-ok assertion (safe).
	s, ok := i.(string)
	chkStr("assert/ok-value", s, "") // zero on failure
	chk("assert/ok-flag", ok, false)
	n2, ok2 := i.(int)
	chk("assert/ok-success-value", n2, 42)
	chkTrue("assert/ok-success-flag", ok2)

	// Assert to interface type.
	var w any = widget{id: 1}
	if st, ok := w.(fmt.Stringer); ok {
		chkStr("assert/to-iface", st.String(), "w1")
	} else {
		chk("assert/to-iface", 0, 1) // force fail if not matched
	}

	// Type switch over many dynamic types.
	chkStr("tswitch/nil", classify(nil), "nil")
	chkStr("tswitch/int", classify(7), "int:7")
	chkStr("tswitch/string", classify("hello"), "string:5")
	chkStr("tswitch/bool", classify(true), "bool:true")
	chkStr("tswitch/slice", classify([]int{1, 2, 3}), "[]int:6")
	chkStr("tswitch/stringer", classify(widget{id: 4}), "stringer:w4")
	chkStr("tswitch/default", classify(3.14), "other:float64")
}

// ---------------------------------------------------------------------------
// GENERICS (spec: Type parameters, Constraints, Inference).
// ---------------------------------------------------------------------------

// Ordered constraint via union of underlying types.
type ordered interface {
	~int | ~int8 | ~int16 | ~int32 | ~int64 |
		~uint | ~uint8 | ~uint16 | ~uint32 | ~uint64 | ~uintptr |
		~float32 | ~float64 | ~string
}

// numeric constraint for arithmetic.
type numericC interface {
	~int | ~int64 | ~float64
}

// GMax returns the larger of two ordered values (generic, inferred).
func GMax[T ordered](a, b T) T {
	if a > b {
		return a
	}
	return b
}

// GSum sums a slice of any numeric type.
func GSum[T numericC](xs []T) T {
	var s T
	for _, x := range xs {
		s += x
	}
	return s
}

// GMap is a generic map/transform over slices (two type params).
func GMap[T, U any](xs []T, f func(T) U) []U {
	out := make([]U, len(xs))
	for i, x := range xs {
		out[i] = f(x)
	}
	return out
}

// GFilter keeps elements satisfying a predicate.
func GFilter[T any](xs []T, keep func(T) bool) []T {
	var out []T
	for _, x := range xs {
		if keep(x) {
			out = append(out, x)
		}
	}
	return out
}

// GKeys returns sorted keys of a map (comparable+ordered key).
func GKeys[K ordered, V any](m map[K]V) []K {
	out := make([]K, 0, len(m))
	for k := range m {
		out = append(out, k)
	}
	sort.Slice(out, func(i, j int) bool { return out[i] < out[j] })
	return out
}

// Stack[T] is a generic type with generic methods.
type Stack[T any] struct{ items []T }

func (s *Stack[T]) Push(v T) { s.items = append(s.items, v) }
func (s *Stack[T]) Pop() (T, bool) {
	var zero T
	if len(s.items) == 0 {
		return zero, false
	}
	v := s.items[len(s.items)-1]
	s.items = s.items[:len(s.items)-1]
	return v, true
}
func (s *Stack[T]) Len() int { return len(s.items) }

// Pair is a generic struct with two type params and a method returning a
// type-swapped pair.
type Pair[A, B any] struct {
	First  A
	Second B
}

func (p Pair[A, B]) Swap() Pair[B, A] { return Pair[B, A]{p.Second, p.First} }

func runGenerics() {
	section("generics")

	// Type inference (no explicit type args).
	chk("gen/max-int", GMax(3, 9), 9)
	chk("gen/max-float", GMax(2.5, 1.5), 2.5)
	chkStr("gen/max-string", GMax("apple", "banana"), "banana")

	// Explicit instantiation.
	chk("gen/max-explicit", GMax[int](10, 4), 10)

	// Sum over int and float64 (constraint union).
	chk("gen/sum-int", GSum([]int{1, 2, 3, 4, 5}), 15)
	chk("gen/sum-float", GSum([]float64{0.5, 0.25, 0.125}), 0.875)

	// Defined type with underlying int satisfies ~int.
	type myInt int
	chk("gen/sum-defined", GSum([]myInt{10, 20}), myInt(30))

	// Generic map/transform: ints -> doubled, summed.
	doubled := GMap([]int{1, 2, 3}, func(x int) int { return x * 2 })
	chk("gen/map-sum", GSum(doubled), 12)
	// Cross-type transform: int -> string lengths.
	strs := GMap([]int{1, 22, 333}, func(x int) string { return fmt.Sprint(x) })
	chkStr("gen/map-crosstype", strings.Join(strs, ","), "1,22,333")

	// Generic filter.
	evens := GFilter([]int{1, 2, 3, 4, 5, 6}, func(x int) bool { return x%2 == 0 })
	chk("gen/filter-sum", GSum(evens), 12)

	// Generic sorted keys.
	keys := GKeys(map[string]int{"b": 2, "a": 1, "c": 3})
	chkStr("gen/keys-sorted", strings.Join(keys, ""), "abc")

	// Generic type with generic methods.
	var st Stack[string]
	st.Push("x")
	st.Push("y")
	st.Push("z")
	chk("gen/stack-len", st.Len(), 3)
	top, ok := st.Pop()
	chkTrue("gen/stack-pop-ok", ok)
	chkStr("gen/stack-pop", top, "z")
	chk("gen/stack-len-after", st.Len(), 2)

	// Generic struct + type-swapping method.
	p := Pair[int, string]{First: 1, Second: "one"}
	swapped := p.Swap()
	chkStr("gen/pair-swap-first", swapped.First, "one")
	chk("gen/pair-swap-second", swapped.Second, 1)

	// comparable constraint via a generic Contains.
	chkTrue("gen/contains-true", gContains([]int{1, 2, 3}, 2))
	chk("gen/contains-false", gContains([]string{"a", "b"}, "z"), false)
}

// gContains uses the comparable predeclared constraint.
func gContains[T comparable](xs []T, want T) bool {
	for _, x := range xs {
		if x == want {
			return true
		}
	}
	return false
}
