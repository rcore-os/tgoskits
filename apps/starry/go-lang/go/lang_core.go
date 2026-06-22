package main

import (
	"fmt"
	"math"
	"strconv"
	"strings"
	"unicode/utf8"
)

// ---------------------------------------------------------------------------
// BASIC TYPES + CONVERSIONS (spec: Types, Conversions, Numeric).
// ---------------------------------------------------------------------------

func runBasicTypes() {
	section("basic-types")

	// Boolean.
	var b bool = true
	chkTrue("bool/true", b)
	chk("bool/and", b && false, false)

	// Sized integers: ranges via math constants.
	chk("int8/max", int8(math.MaxInt8), int8(127))
	chk("int8/min", int8(math.MinInt8), int8(-128))
	chk("uint8/max", uint8(math.MaxUint8), uint8(255))
	chk("int16/max", int16(math.MaxInt16), int16(32767))
	chk("uint16/max", uint16(math.MaxUint16), uint16(65535))
	chk("int32/max", int32(math.MaxInt32), int32(2147483647))
	chk("uint32/max", uint32(math.MaxUint32), uint32(4294967295))
	chk("int64/max", int64(math.MaxInt64), int64(9223372036854775807))
	chk("uint64/max", uint64(math.MaxUint64), uint64(18446744073709551615))

	// Unsigned wraparound is defined modular arithmetic (use vars: a constant
	// 255+1 would be a compile-time overflow, so force a runtime add).
	var u8a, u8b uint8 = 255, 1
	chk("uint8/wrap", u8a+u8b, uint8(0))
	var i200 int = 200
	chk("int8/overflow-conv", int8(i200), int8(-56))

	// byte and rune are aliases.
	chk("byte=uint8", byte('A'), uint8(65))
	chk("rune=int32", rune('世'), int32(19990))

	// Floating point and complex.
	var f32 float32 = 1.0 / 3.0
	chkStr("float32/str", strconv.FormatFloat(float64(f32), 'g', 7, 32), "0.3333333")
	chk("float64/div", 1.0/4.0, 0.25)
	c := complex(3, 4)
	chk("complex/real", real(c), float64(3))
	chk("complex/imag", imag(c), float64(4))
	chk("complex/abs", math.Hypot(real(c), imag(c)), float64(5))
	chk("complex64/conv", complex64(c), complex64(complex(3, 4)))

	// uintptr width is platform-dependent but conversion round-trips a small int.
	chk("uintptr/roundtrip", int(uintptr(42)), 42)

	// Conversions: numeric, string<->[]byte, string<->[]rune, int<->string(rune).
	chk("conv/int->float", float64(7)/2, 3.5)
	var f399 float64 = 3.99
	chk("conv/float->int-trunc", int(f399), 3)
	chkStr("conv/[]byte->string", string([]byte{0x47, 0x6f}), "Go")
	chk("conv/string->[]byte/len", len([]byte("héllo")), 6) // é is 2 bytes
	chk("conv/string->[]rune/len", len([]rune("héllo")), 5)
	chkStr("conv/rune->string", string(rune(0x4e16)), "世")
	chk("conv/utf8.RuneCountInString", utf8.RuneCountInString("héllo世"), 6)

	// Zero values.
	var zi int
	var zs string
	var zb bool
	var zf float64
	var zp *int
	chk("zero/int", zi, 0)
	chkStr("zero/string", zs, "")
	chk("zero/bool", zb, false)
	chk("zero/float", zf, 0.0)
	chkTrue("zero/pointer-nil", zp == nil)

	// Named type with underlying conversion.
	type Celsius float64
	type Fahrenheit float64
	cTemp := Celsius(100)
	fTemp := Fahrenheit(float64(cTemp)*9/5 + 32)
	chk("named/celsius->fahrenheit", float64(fTemp), 212.0)
}

// ---------------------------------------------------------------------------
// CONSTANTS + IOTA (spec: Constants, Iota).
// ---------------------------------------------------------------------------

// iota: simple successive constants.
const (
	cZero = iota // 0
	cOne         // 1
	cTwo         // 2
	cThree       // 3
)

// iota: bit-shift flags (classic pattern).
const (
	flagA = 1 << iota // 1
	flagB             // 2
	flagC             // 4
	flagD             // 8
)

// iota: skip with blank, and resume.
const (
	_      = iota             // skip 0
	KB     = 1 << (10 * iota) // 1<<10
	MB                        // 1<<20
	GB                        // 1<<30
)

// iota: mixed with explicit values; iota keeps incrementing.
const (
	em0 = iota * 10 // 0
	em1             // 10
	em2 = 99        // 99 (explicit)
	em3 = iota * 10 // 30 (iota is now 3)
)

// Typed and untyped constants.
const (
	typedPi   float64 = 3.14159
	untypedPi         = 3.14159
	bigShift          = 1 << 62
	maxIntC           = 1<<63 - 1
)

func runConstantsIota() {
	section("constants-iota")

	chk("iota/zero", cZero, 0)
	chk("iota/one", cOne, 1)
	chk("iota/two", cTwo, 2)
	chk("iota/three", cThree, 3)

	chk("iota/flagA", flagA, 1)
	chk("iota/flagB", flagB, 2)
	chk("iota/flagC", flagC, 4)
	chk("iota/flagD", flagD, 8)
	chk("iota/flag-combine", flagA|flagC, 5)

	chk("iota/KB", KB, 1024)
	chk("iota/MB", MB, 1048576)
	chk("iota/GB", GB, 1073741824)

	chk("iota/mixed-em0", em0, 0)
	chk("iota/mixed-em1", em1, 10)
	chk("iota/mixed-em2", em2, 99)
	chk("iota/mixed-em3", em3, 30)

	chk("const/typed-pi", typedPi, 3.14159)
	// Untyped const adapts: usable as float and as truncated int context.
	chk("const/untyped-as-float", untypedPi*2, 6.28318)
	chk("const/bigShift", bigShift, 4611686018427387904)
	chk("const/maxInt", maxIntC, int(math.MaxInt64))

	// Untyped constant high precision: representable exactly before assignment.
	const huge = 1 << 100
	chk("const/huge>>100", huge>>100, 1)
}

// ---------------------------------------------------------------------------
// OPERATORS (spec: Operators, precedence).
// ---------------------------------------------------------------------------

func runOperators() {
	section("operators")

	// Arithmetic.
	chk("op/add", 7+3, 10)
	chk("op/sub", 7-3, 4)
	chk("op/mul", 7*3, 21)
	chk("op/quo", 7/3, 2)
	chk("op/rem", 7%3, 1)
	chk("op/neg-rem", -7%3, -1) // truncated toward zero

	// Bitwise.
	chk("op/and", 0b1100&0b1010, 0b1000)
	chk("op/or", 0b1100|0b1010, 0b1110)
	chk("op/xor", 0b1100^0b1010, 0b0110)
	chk("op/andnot", 0b1100&^0b1010, 0b0100) // AND NOT (bit clear)
	chk("op/shl", 1<<4, 16)
	chk("op/shr", 256>>4, 16)
	chk("op/complement", ^uint8(0), uint8(255))

	// Comparison.
	chkTrue("op/eq", 3 == 3)
	chkTrue("op/ne", 3 != 4)
	chkTrue("op/lt", 3 < 4)
	chkTrue("op/le", 3 <= 3)
	chkTrue("op/gt", 4 > 3)
	chkTrue("op/ge", 4 >= 4)

	// Logical short-circuit (record side-effect order deterministically).
	calls := 0
	sideTrue := func() bool { calls++; return true }
	_ = false && sideTrue()                      // short-circuits: not called
	chk("op/&&-shortcircuit", calls, 0)          // RHS skipped
	_ = true || sideTrue()                       // short-circuits: not called
	chk("op/||-shortcircuit", calls, 0)          // RHS skipped
	chkTrue("op/||-eval", true || sideTrue())    // still true, still skipped
	chk("op/sideeffect-count", calls, 0)

	// Precedence: * binds tighter than +, << tighter than +.
	chk("op/precedence-1", 2+3*4, 14)
	chk("op/precedence-2", 1<<2+1, 5)   // (1<<2)+1 because << > +
	chk("op/precedence-paren", (2+3)*4, 20)

	// Assignment operators.
	x := 10
	x += 5
	chk("op/+=", x, 15)
	x -= 3
	chk("op/-=", x, 12)
	x *= 2
	chk("op/*=", x, 24)
	x /= 4
	chk("op//=", x, 6)
	x %= 4
	chk("op/%=", x, 2)
	x <<= 3
	chk("op/<<=", x, 16)
	x >>= 1
	chk("op/>>=", x, 8)
	x &= 0b1010
	chk("op/&=", x, 8)
	x |= 0b0001
	chk("op/|=", x, 9)
	x ^= 0b0011
	chk("op/^=", x, 10)
	x &^= 0b0010
	chk("op/&^=", x, 8)

	// Increment / decrement statements.
	y := 5
	y++
	chk("op/++", y, 6)
	y--
	chk("op/--", y, 5)

	// Pointer address / deref.
	p := &y
	*p = 42
	chk("op/deref-assign", y, 42)
	chk("op/addr-deref", *p, 42)
}

// ---------------------------------------------------------------------------
// STRINGS / RUNES / BYTES (spec: String types; pkg strings/strconv).
// ---------------------------------------------------------------------------

func runStringsRunesBytes() {
	section("strings-runes-bytes")

	s := "héllo, 世界"
	chk("str/len-bytes", len(s), 14)                      // bytes: h é(2) l l o , sp 世(3) 界(3)
	chk("str/runecount", utf8.RuneCountInString(s), 9)    // runes
	chk("str/index-byte", s[0], byte('h'))                // byte indexing
	chkStr("str/slice-bytes", s[0:5], "héll")             // byte slice (é=2 bytes)
	chkStr("str/concat", "go"+"-"+"os", "go-os")

	// Raw vs interpreted string literals.
	interp := "a\tb\nc"
	raw := `a\tb\nc`
	chk("str/interp-len", len(interp), 5)
	chk("str/raw-len", len(raw), 7) // a \ t b \ n c — backslashes are literal
	chkStr("str/raw-content", raw, `a\tb\nc`)

	// Iterating a string yields (byteIndex, rune); sum runes order-independently.
	var runeSum, runeN int
	var firstIdx, lastIdx int = -1, -1
	for i, r := range s {
		if firstIdx == -1 {
			firstIdx = i
		}
		lastIdx = i
		runeSum += int(r)
		runeN++
	}
	chk("str/range-runeN", runeN, 9)
	chk("str/range-firstidx", firstIdx, 0)
	chk("str/range-lastidx", lastIdx, 11) // byte offset of '界' (h0 é1 l3 l4 o5 ,6 sp7 世8 界11)
	chk("str/range-runesum", runeSum, int('h'+'é'+'l'+'l'+'o'+','+' '+'世'+'界'))

	// Rune literals and escapes.
	chk("rune/lit", 'A', rune(65))
	chk("rune/unicode-escape", '世', rune(0x4e16))
	chk("rune/hex-escape", '\x41', rune(65))
	chk("rune/newline", '\n', rune(10))

	// []rune round-trip.
	rs := []rune(s)
	chk("rune/slice-len", len(rs), 9)
	chkStr("rune/slice-roundtrip", string(rs), s)

	// strings.Builder (efficient concatenation, deterministic).
	var sb strings.Builder
	for i := 0; i < 3; i++ {
		fmt.Fprintf(&sb, "%d;", i)
	}
	chkStr("str/builder", sb.String(), "0;1;2;")
	chk("str/builder-len", sb.Len(), 6)
}
