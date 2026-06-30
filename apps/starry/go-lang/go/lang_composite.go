package main

import (
	"fmt"
	"sort"
	"strings"
)

// ---------------------------------------------------------------------------
// ARRAYS + SLICES (spec: Array types, Slice types, append/copy/clear).
// ---------------------------------------------------------------------------

func runArraysSlices() {
	section("arrays-slices")

	// Arrays are value types of fixed length; assignment copies.
	var arr [5]int
	for i := range arr {
		arr[i] = i * i
	}
	chk("arr/len", len(arr), 5)
	chk("arr/elem", arr[3], 9)
	arrCopy := arr // value copy
	arrCopy[0] = 99
	chk("arr/value-semantics", arr[0], 0) // original unchanged
	chk("arr/comparable", arr == [5]int{0, 1, 4, 9, 16}, true)

	// Array literal with index keys.
	sparse := [5]int{2: 20, 4: 40}
	chk("arr/index-literal", sparse[2], 20)
	chk("arr/index-literal-zero", sparse[1], 0)

	// 2D array.
	var grid [2][3]int
	grid[1][2] = 7
	chk("arr/2d", grid[1][2], 7)

	// Slices: literal, len, cap.
	s := []int{1, 2, 3, 4, 5}
	chk("slice/len", len(s), 5)
	chk("slice/cap>=len", cap(s) >= 5, true)

	// append: growing past cap reallocates.
	s = append(s, 6, 7)
	chk("slice/append-len", len(s), 7)
	chk("slice/append-elem", s[6], 7)
	// append a slice with ... spread.
	s = append(s, []int{8, 9}...)
	chk("slice/append-spread", s[8], 9)
	chk("slice/append-spread-len", len(s), 9)

	// copy: returns number of elements copied (min of lens).
	dst := make([]int, 3)
	n := copy(dst, s[1:4]) // copies 2,3,4
	chk("slice/copy-n", n, 3)
	chk("slice/copy-head", dst[0], 2)
	chk("slice/copy-tail", dst[2], 4)

	// 2-index slice expression: shares backing array.
	sub := s[2:5]
	chk("slice/2idx-len", len(sub), 3)
	chk("slice/2idx-elem0", sub[0], 3)
	sub[0] = 100
	chk("slice/2idx-aliases-backing", s[2], 100) // mutation visible
	s[2] = 3                                      // restore

	// 3-index full slice expression: a[low:high:max] caps capacity.
	full := s[1:3:4]
	chk("slice/3idx-len", len(full), 2)
	chk("slice/3idx-cap", cap(full), 3) // max-low = 4-1
	// append within cap stays in backing; we don't assert aliasing to keep determinism.

	// clear() built-in zeroes a slice's elements in place (Go 1.21+).
	cl := []int{9, 8, 7}
	clear(cl)
	chkStr("slice/clear", fmt.Sprint(cl), "[0 0 0]")
	chk("slice/clear-keeps-len", len(cl), 3)

	// make with len and cap.
	m1 := make([]int, 2, 8)
	chk("slice/make-len", len(m1), 2)
	chk("slice/make-cap", cap(m1), 8)

	// nil slice is usable: len 0, appendable.
	var nilSlice []int
	chk("slice/nil-len", len(nilSlice), 0)
	chkTrue("slice/nil-is-nil", nilSlice == nil)
	nilSlice = append(nilSlice, 1)
	chk("slice/nil-append", nilSlice[0], 1)

	// Sum a slice order-independently (deterministic regardless of layout).
	total := 0
	for _, v := range []int{10, 20, 30, 40} {
		total += v
	}
	chk("slice/range-sum", total, 100)

	// Slice of slices (jagged) joined deterministically.
	jag := [][]string{{"a", "b"}, {"c"}}
	var parts []string
	for _, row := range jag {
		parts = append(parts, strings.Join(row, ""))
	}
	chkStr("slice/jagged-join", strings.Join(parts, "|"), "ab|c")
}

// ---------------------------------------------------------------------------
// MAPS (spec: Map types, delete, clear).
// ---------------------------------------------------------------------------

func runMaps() {
	section("maps")

	m := map[string]int{"a": 1, "b": 2, "c": 3}
	chk("map/len", len(m), 3)
	chk("map/index", m["b"], 2)

	// comma-ok existence check.
	v, ok := m["c"]
	chk("map/ok-value", v, 3)
	chkTrue("map/ok-present", ok)
	_, ok2 := m["missing"]
	chk("map/ok-absent", ok2, false)

	// Indexing a missing key yields the zero value.
	chk("map/missing-zero", m["zzz"], 0)

	// Insert + update.
	m["d"] = 4
	chk("map/insert", m["d"], 4)
	m["a"] = 10
	chk("map/update", m["a"], 10)

	// delete().
	delete(m, "b")
	_, gone := m["b"]
	chk("map/delete", gone, false)
	chk("map/delete-len", len(m), 3) // a,c,d

	// Deterministic readback: collect + sort keys.
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	var dump strings.Builder
	for _, k := range keys {
		fmt.Fprintf(&dump, "%s=%d;", k, m[k])
	}
	chkStr("map/sorted-dump", dump.String(), "a=10;c=3;d=4;")

	// Sum of values is order-independent.
	sum := 0
	for _, val := range m {
		sum += val
	}
	chk("map/value-sum", sum, 17)

	// clear() empties a map (Go 1.21+).
	clear(m)
	chk("map/clear", len(m), 0)

	// Map with struct values; comparable struct keys.
	type point struct{ x, y int }
	grid := map[point]string{{0, 0}: "origin", {1, 1}: "diag"}
	chkStr("map/struct-key", grid[point{1, 1}], "diag")

	// nil map reads ok (zero), but is not writable — only read here.
	var nm map[string]int
	chk("map/nil-read", nm["x"], 0)
	chk("map/nil-len", len(nm), 0)

	// Set idiom: map[T]struct{} with sorted readback.
	set := map[string]struct{}{}
	for _, w := range []string{"x", "y", "x", "z", "y"} {
		set[w] = struct{}{}
	}
	chk("map/set-distinct", len(set), 3)
	setKeys := make([]string, 0, len(set))
	for k := range set {
		setKeys = append(setKeys, k)
	}
	sort.Strings(setKeys)
	chkStr("map/set-sorted", strings.Join(setKeys, ","), "x,y,z")
}

// ---------------------------------------------------------------------------
// STRUCTS + EMBEDDING + TAGS (spec: Struct types).
// ---------------------------------------------------------------------------

type baseT struct {
	ID   int
	Name string
}

func (b baseT) Describe() string { return fmt.Sprintf("#%d:%s", b.ID, b.Name) }

// derivedT embeds baseT (promotes its fields and methods).
type derivedT struct {
	baseT        // embedded (anonymous) field
	Extra string `json:"extra" custom:"tagval"`
}

// deepT embeds derivedT (two-level promotion).
type deepT struct {
	derivedT
	Depth int
}

func runStructsEmbedding() {
	section("structs-embedding")

	// Struct literal: field names and positional.
	b := baseT{ID: 7, Name: "go"}
	chk("struct/field", b.ID, 7)
	chkStr("struct/field-name", b.Name, "go")
	chkStr("struct/method", b.Describe(), "#7:go")

	// Positional literal.
	bp := baseT{9, "os"}
	chk("struct/positional", bp.ID, 9)

	// Comparable structs compare field-by-field.
	chkTrue("struct/equal", baseT{1, "x"} == baseT{1, "x"})
	chkTrue("struct/notequal", baseT{1, "x"} != baseT{2, "x"})

	// Pointer to struct: auto-deref on field access.
	pb := &b
	pb.ID = 42
	chk("struct/ptr-field", b.ID, 42)

	// Embedding: promoted field + promoted method.
	d := derivedT{baseT: baseT{ID: 1, Name: "embed"}, Extra: "ex"}
	chk("embed/promoted-field", d.ID, 1)           // d.baseT.ID
	chkStr("embed/promoted-method", d.Describe(), "#1:embed")
	chkStr("embed/own-field", d.Extra, "ex")
	chkStr("embed/explicit-path", d.baseT.Name, "embed")

	// Two-level embedding.
	dd := deepT{derivedT: derivedT{baseT: baseT{ID: 5, Name: "deep"}}, Depth: 3}
	chk("embed/2level-field", dd.ID, 5)
	chkStr("embed/2level-method", dd.Describe(), "#5:deep")
	chk("embed/2level-own", dd.Depth, 3)

	// Struct tags via reflection-free string check is not possible; assert via
	// reflect in runStdReflect. Here assert anonymous struct + comparison.
	anon := struct {
		A int
		B string
	}{A: 1, B: "z"}
	chk("struct/anon-field", anon.A, 1)
	chkStr("struct/anon-name", anon.B, "z")

	// Anonymous struct slice (table-driven style) summed deterministically.
	rows := []struct {
		k string
		v int
	}{{"a", 1}, {"b", 2}, {"c", 3}}
	sum := 0
	for _, r := range rows {
		sum += r.v
	}
	chk("struct/anon-slice-sum", sum, 6)

	// Empty struct has size 0 — used as a set value / signal.
	var sig struct{}
	_ = sig
	chk("struct/empty-size", int(unsafeSizeofEmpty()), 0)
}
