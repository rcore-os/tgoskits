package main

import (
	"bytes"
	"crypto/sha3"
	"encoding/hex"
	"errors"
	"fmt"
	"log/slog"
	"net/netip"
	"reflect"
	"strings"
)

// ---------------------------------------------------------------------------
// GO 1.26 SPECIFICS (verified against go.dev/doc/go1.26 release notes and the
// go1.26.3 toolchain). Each item is grounded in a release-note line.
// ---------------------------------------------------------------------------

// --- Language: self-referential generic type parameter `[A Adder[A]]` --------
type Adder[A Adder[A]] interface{ Add(A) A }

type addInt int

func (a addInt) Add(b addInt) addInt { return a + b }

func algoAdd[A Adder[A]](x, y A) A { return x.Add(y) }

// --- Language: new() with an EXPRESSION operand ------------------------------
// Pre-1.26 new() took only a type; 1.26 allows new(expr) to allocate + store.
func yearsSince(born int) int { return 2026 - born }

// --- Stdlib: errors.AsType[E] — generic, type-safe errors.As -----------------
type codeErr126 struct{ code int }

func (e *codeErr126) Error() string { return fmt.Sprintf("codeErr(%d)", e.code) }

func runGo126Features() {
	section("go1.26-features")

	// LANGUAGE: new(expr) — allocate a *int initialized from an expression.
	age := new(yearsSince(2000)) // *int -> 26
	chk("go126/new-expr", *age, 26)
	// new(expr) with a literal expression.
	p := new(40 + 2)
	chk("go126/new-expr-literal", *p, 42)
	// Type of new(expr) is *T of the expression's type.
	chkStr("go126/new-expr-type", fmt.Sprintf("%T", age), "*int")

	// LANGUAGE: self-referential generic type parameter.
	chk("go126/self-generic", algoAdd(addInt(3), addInt(4)), addInt(7))

	// STDLIB: errors.AsType[E] — generic type-safe extraction from a chain.
	chain := fmt.Errorf("deep: %w", &codeErr126{code: 99})
	if e, ok := errors.AsType[*codeErr126](chain); ok {
		chk("go126/errors.AsType-code", e.code, 99)
	} else {
		chk("go126/errors.AsType-code", 0, 1) // force fail
	}
	// AsType miss on an unrelated target type.
	_, miss := errors.AsType[*panicErr](chain)
	chk("go126/errors.AsType-miss", boolToInt(miss), 0)

	// STDLIB: bytes.Buffer.Peek(n) — read n bytes WITHOUT advancing.
	var pb bytes.Buffer
	pb.WriteString("starry")
	pk, perr := pb.Peek(3)
	chkStr("go126/bytes.Peek", string(pk), "sta")
	chk("go126/bytes.Peek-noerr", boolToInt(perr == nil), 1)
	chk("go126/bytes.Peek-len-unchanged", pb.Len(), 6) // not advanced
	// Peek more than available -> returns what it can + io.EOF.
	pk2, perr2 := pb.Peek(100)
	chkStr("go126/bytes.Peek-overflow", string(pk2), "starry")
	chk("go126/bytes.Peek-eof", boolToInt(perr2 != nil), 1)

	// STDLIB: net/netip.Prefix.Compare — ordered prefix comparison.
	// /8 sorts before /16 at the same masked address -> Compare returns -1.
	c1 := netip.MustParsePrefix("10.0.0.0/8").Compare(netip.MustParsePrefix("10.0.0.0/16"))
	chk("go126/netip.Prefix.Compare-lt", c1, -1)
	c2 := netip.MustParsePrefix("10.0.0.0/16").Compare(netip.MustParsePrefix("10.0.0.0/8"))
	chk("go126/netip.Prefix.Compare-gt", c2, 1)
	c3 := netip.MustParsePrefix("10.0.0.0/24").Compare(netip.MustParsePrefix("10.0.0.0/24"))
	chk("go126/netip.Prefix.Compare-eq", c3, 0)

	// STDLIB: log/slog.NewMultiHandler — fan one record out to N handlers.
	var lb1, lb2 bytes.Buffer
	strip := &slog.HandlerOptions{
		ReplaceAttr: func(g []string, a slog.Attr) slog.Attr {
			if a.Key == slog.TimeKey {
				return slog.Attr{}
			}
			return a
		},
	}
	mh := slog.NewMultiHandler(
		slog.NewTextHandler(&lb1, strip),
		slog.NewTextHandler(&lb2, strip),
	)
	slog.New(mh).Info("hi", "k", "v")
	chkTrue("go126/slog.MultiHandler-sink1", strings.Contains(lb1.String(), "msg=hi"))
	chkTrue("go126/slog.MultiHandler-sink2", strings.Contains(lb2.String(), "msg=hi"))
	chkTrue("go126/slog.MultiHandler-attr", strings.Contains(lb1.String(), "k=v") && strings.Contains(lb2.String(), "k=v"))

	// STDLIB: reflect.Type.Fields / Value.Fields — iterators over struct fields.
	type rec struct {
		A int
		B string
	}
	var fieldNames []string
	for f := range reflect.TypeOf(rec{}).Fields() {
		fieldNames = append(fieldNames, f.Name)
	}
	chkStr("go126/reflect.Type.Fields", strings.Join(fieldNames, ","), "A,B")
	// Value.Fields yields (StructField, Value) pairs.
	var pairs []string
	for f, val := range reflect.ValueOf(rec{A: 7, B: "x"}).Fields() {
		pairs = append(pairs, fmt.Sprintf("%s=%v", f.Name, val.Interface()))
	}
	chkStr("go126/reflect.Value.Fields", strings.Join(pairs, ","), "A=7,B=x")
	// reflect.Type.Ins / Outs — iterators over function params.
	ft := reflect.TypeOf(func(int, string) (bool, error) { return false, nil })
	var ins []string
	for in := range ft.Ins() {
		ins = append(ins, in.Kind().String())
	}
	chkStr("go126/reflect.Type.Ins", strings.Join(ins, ","), "int,string")
	var outs []string
	for out := range ft.Outs() {
		outs = append(outs, out.Kind().String())
	}
	chkStr("go126/reflect.Type.Outs", strings.Join(outs, ","), "bool,interface")

	// STDLIB: crypto/sha3 zero value is a usable SHA3-256 instance (Go 1.26).
	var h sha3.SHA3 // zero value usable as SHA3-256
	h.Write([]byte("abc"))
	chkStr("go126/sha3.zero-value-sha3-256", hex.EncodeToString(h.Sum(nil)),
		"3a985da74fe225b2045c172d6bd390bd855f086e3e9d525b46bfe24511431532")
	// SHA3-256 helper API (also present) cross-check.
	helperSum := sha3.Sum256([]byte("abc"))
	chkStr("go126/sha3.Sum256-cross", hex.EncodeToString(helperSum[:]),
		"3a985da74fe225b2045c172d6bd390bd855f086e3e9d525b46bfe24511431532")
}
