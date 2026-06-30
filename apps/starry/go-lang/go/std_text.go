package main

import (
	"bytes"
	"fmt"
	"strconv"
	"strings"
	"unicode"
	"unicode/utf8"
)

// ---------------------------------------------------------------------------
// fmt + strconv.
// ---------------------------------------------------------------------------

type fmtStringer struct{ x int }

func (f fmtStringer) String() string { return fmt.Sprintf("S<%d>", f.x) }

func runStdFmtStrconv() {
	section("std-fmt-strconv")

	// fmt verbs: integers, width, base, sign.
	chkStr("fmt/%d", fmt.Sprintf("%d", 42), "42")
	chkStr("fmt/%5d", fmt.Sprintf("%5d", 42), "   42")
	chkStr("fmt/%-5d|", fmt.Sprintf("%-5d|", 42), "42   |")
	chkStr("fmt/%05d", fmt.Sprintf("%05d", 42), "00042")
	chkStr("fmt/%+d", fmt.Sprintf("%+d", 42), "+42")
	chkStr("fmt/%x", fmt.Sprintf("%x", 255), "ff")
	chkStr("fmt/%X", fmt.Sprintf("%X", 255), "FF")
	chkStr("fmt/%o", fmt.Sprintf("%o", 8), "10")
	chkStr("fmt/%b", fmt.Sprintf("%b", 5), "101")
	chkStr("fmt/%c", fmt.Sprintf("%c", 0x4e16), "世")
	chkStr("fmt/%q-string", fmt.Sprintf("%q", "hi\n"), `"hi\n"`)
	chkStr("fmt/%q-rune", fmt.Sprintf("%q", 'A'), "'A'")

	// Floats.
	chkStr("fmt/%f", fmt.Sprintf("%f", 3.14159), "3.141590")
	chkStr("fmt/%.2f", fmt.Sprintf("%.2f", 3.14159), "3.14")
	chkStr("fmt/%g", fmt.Sprintf("%g", 0.000012345), "1.2345e-05")
	chkStr("fmt/%e", fmt.Sprintf("%e", 1234.5), "1.234500e+03")
	chkStr("fmt/%8.3f", fmt.Sprintf("%8.3f", 2.5), "   2.500")

	// Strings, bools, %v, %+v, %#v, %T.
	chkStr("fmt/%s", fmt.Sprintf("%s", "go"), "go")
	chkStr("fmt/%v-bool", fmt.Sprintf("%v", true), "true")
	chkStr("fmt/%v-slice", fmt.Sprintf("%v", []int{1, 2, 3}), "[1 2 3]")
	chkStr("fmt/%v-map", fmt.Sprintf("%v", map[string]int{"b": 2, "a": 1}), "map[a:1 b:2]") // sorted keys
	type pt struct {
		X, Y int
	}
	chkStr("fmt/%v-struct", fmt.Sprintf("%v", pt{1, 2}), "{1 2}")
	chkStr("fmt/%+v-struct", fmt.Sprintf("%+v", pt{1, 2}), "{X:1 Y:2}")
	chkStr("fmt/%#v-struct", fmt.Sprintf("%#v", pt{1, 2}), "main.pt{X:1, Y:2}")
	chkStr("fmt/%T", fmt.Sprintf("%T", pt{}), "main.pt")
	chkStr("fmt/%T-slice", fmt.Sprintf("%T", []string{}), "[]string")
	chkStr("fmt/%p-nil", fmt.Sprintf("%v", (*int)(nil)), "<nil>")

	// Stringer is honored by %v / %s.
	chkStr("fmt/stringer", fmt.Sprintf("%v", fmtStringer{x: 9}), "S<9>")

	// Argument index + reuse.
	chkStr("fmt/argindex", fmt.Sprintf("%[2]d-%[1]d", 1, 2), "2-1")

	// Sprint / Sprintln / Errorf.
	chkStr("fmt/Sprint", fmt.Sprint(1, 2, 3), "1 2 3")          // spaces added only when NEITHER adjacent operand is a string
	chkStr("fmt/Sprint-stradj", fmt.Sprint("a", 1, "b"), "a1b") // no space when an adjacent operand IS a string
	chkStr("fmt/Sprintln", fmt.Sprintln("x", "y"), "x y\n")
	chkStr("fmt/Errorf", fmt.Errorf("e=%d", 5).Error(), "e=5")

	// Fprintf into a buffer.
	var fb bytes.Buffer
	fmt.Fprintf(&fb, "%d-%s", 1, "go")
	chkStr("fmt/Fprintf", fb.String(), "1-go")

	// Sscanf parses formatted input.
	var si int
	var ss string
	n, _ := fmt.Sscanf("42 go", "%d %s", &si, &ss)
	chk("fmt/Sscanf-n", n, 2)
	chk("fmt/Sscanf-int", si, 42)
	chkStr("fmt/Sscanf-str", ss, "go")

	// strconv: Atoi/Itoa, ParseInt bases, ParseFloat, ParseBool, Quote.
	ai, _ := strconv.Atoi("12345")
	chk("strconv/Atoi", ai, 12345)
	chkStr("strconv/Itoa", strconv.Itoa(-678), "-678")
	pi, _ := strconv.ParseInt("ff", 16, 64)
	chk("strconv/ParseInt-hex", int(pi), 255)
	pb, _ := strconv.ParseInt("1010", 2, 64)
	chk("strconv/ParseInt-bin", int(pb), 10)
	po, _ := strconv.ParseInt("777", 8, 64)
	chk("strconv/ParseInt-oct", int(po), 511)
	pu, _ := strconv.ParseUint("4294967295", 10, 64)
	chk("strconv/ParseUint", pu, uint64(4294967295))
	pf, _ := strconv.ParseFloat("3.14", 64)
	chk("strconv/ParseFloat", pf, 3.14)
	pbool, _ := strconv.ParseBool("true")
	chkTrue("strconv/ParseBool", pbool)
	chkStr("strconv/FormatInt-hex", strconv.FormatInt(255, 16), "ff")
	chkStr("strconv/FormatFloat", strconv.FormatFloat(2.5, 'f', 2, 64), "2.50")
	chkStr("strconv/Quote", strconv.Quote("a\tb"), `"a\tb"`)
	uq, _ := strconv.Unquote(`"a\tb"`)
	chkStr("strconv/Unquote", uq, "a\tb")
	// Atoi error path.
	_, aerr := strconv.Atoi("xyz")
	chkTrue("strconv/Atoi-err", aerr != nil)
	chkStr("strconv/AppendInt", string(strconv.AppendInt([]byte("n="), 7, 10)), "n=7")
}

// ---------------------------------------------------------------------------
// strings + bytes.
// ---------------------------------------------------------------------------

func runStdStringsBytes() {
	section("std-strings-bytes")

	// strings predicates.
	chkTrue("strings/Contains", strings.Contains("seafood", "foo"))
	chkTrue("strings/HasPrefix", strings.HasPrefix("golang", "go"))
	chkTrue("strings/HasSuffix", strings.HasSuffix("golang", "ng"))
	chk("strings/Index", strings.Index("chicken", "ken"), 4)
	chk("strings/LastIndex", strings.LastIndex("go gopher", "go"), 3)
	chk("strings/Count", strings.Count("cheese", "e"), 3)
	chk("strings/IndexByte", strings.IndexByte("golang", 'a'), 3)
	chk("strings/IndexRune", strings.IndexRune("chicken", 'k'), 4)
	chkTrue("strings/ContainsRune", strings.ContainsRune("aardvark", 'v'))
	chkTrue("strings/ContainsAny", strings.ContainsAny("hello", "xyz e"))

	// case + transformations.
	chkStr("strings/ToUpper", strings.ToUpper("go"), "GO")
	chkStr("strings/ToLower", strings.ToLower("GO"), "go")
	chkStr("strings/Title-via-ToTitle", strings.ToTitle("loud"), "LOUD")
	chkStr("strings/TrimSpace", strings.TrimSpace("  hi \n"), "hi")
	chkStr("strings/Trim", strings.Trim("xxhixx", "x"), "hi")
	chkStr("strings/TrimLeft", strings.TrimLeft("xxhi", "x"), "hi")
	chkStr("strings/TrimRight", strings.TrimRight("hixx", "x"), "hi")
	chkStr("strings/TrimPrefix", strings.TrimPrefix("gopher", "go"), "pher")
	chkStr("strings/TrimSuffix", strings.TrimSuffix("gopher", "her"), "gop")
	chkStr("strings/Replace", strings.Replace("oink oink", "k", "ky", 1), "oinky oink")
	chkStr("strings/ReplaceAll", strings.ReplaceAll("a-b-c", "-", "+"), "a+b+c")
	chkStr("strings/Repeat", strings.Repeat("ab", 3), "ababab")
	chkStr("strings/Map", strings.Map(func(r rune) rune { return r + 1 }, "abc"), "bcd")

	// split / join / fields.
	chkStr("strings/Split", fmt.Sprint(strings.Split("a,b,c", ",")), "[a b c]")
	chkStr("strings/SplitN", fmt.Sprint(strings.SplitN("a,b,c", ",", 2)), "[a b,c]")
	chkStr("strings/Fields", fmt.Sprint(strings.Fields("  a  b\tc ")), "[a b c]")
	chkStr("strings/Join", strings.Join([]string{"x", "y", "z"}, "-"), "x-y-z")
	chkStr("strings/Cut-before", cutBefore("k=v"), "k")
	chkStr("strings/Cut-after", cutAfter("k=v"), "v")

	// EqualFold, Compare.
	chkTrue("strings/EqualFold", strings.EqualFold("Go", "GO"))
	chk("strings/Compare-lt", strings.Compare("a", "b"), -1)
	chk("strings/Compare-eq", strings.Compare("a", "a"), 0)
	chk("strings/Compare-gt", strings.Compare("b", "a"), 1)

	// strings.NewReplacer.
	rep := strings.NewReplacer("<", "&lt;", ">", "&gt;")
	chkStr("strings/Replacer", rep.Replace("a<b>c"), "a&lt;b&gt;c")

	// strings.Builder + Reader.
	var sb strings.Builder
	sb.WriteString("ab")
	sb.WriteByte('c')
	sb.WriteRune('世')
	chkStr("strings/Builder", sb.String(), "abc世")
	rd := strings.NewReader("hello")
	chk("strings/Reader-len", rd.Len(), 5)
	bb := make([]byte, 3)
	rn, _ := rd.Read(bb)
	chk("strings/Reader-read-n", rn, 3)
	chkStr("strings/Reader-read", string(bb), "hel")

	// bytes mirror of strings.
	chkTrue("bytes/Contains", bytes.Contains([]byte("seafood"), []byte("foo")))
	chk("bytes/Index", bytes.Index([]byte("chicken"), []byte("ken")), 4)
	chkStr("bytes/ToUpper", string(bytes.ToUpper([]byte("go"))), "GO")
	chkStr("bytes/Join", string(bytes.Join([][]byte{[]byte("a"), []byte("b")}, []byte("-"))), "a-b")
	chkStr("bytes/Split", fmt.Sprint(bytes.Split([]byte("a,b"), []byte(","))), "[[97] [98]]")
	chk("bytes/Equal", boolToInt(bytes.Equal([]byte("x"), []byte("x"))), 1)
	chk("bytes/Compare", bytes.Compare([]byte("a"), []byte("b")), -1)
	chkStr("bytes/TrimSpace", string(bytes.TrimSpace([]byte(" hi "))), "hi")
	chk("bytes/Count", bytes.Count([]byte("banana"), []byte("a")), 3)

	// bytes.Buffer write/read.
	var buf bytes.Buffer
	buf.WriteString("hello ")
	buf.WriteString("world")
	chk("bytes/Buffer-len", buf.Len(), 11)
	chkStr("bytes/Buffer-string", buf.String(), "hello world")
	got := make([]byte, 5)
	bn, _ := buf.Read(got)
	chk("bytes/Buffer-read-n", bn, 5)
	chkStr("bytes/Buffer-read", string(got), "hello")
	chk("bytes/Buffer-len-after-read", buf.Len(), 6)
}

func cutBefore(s string) string { b, _, _ := strings.Cut(s, "="); return b }
func cutAfter(s string) string  { _, a, _ := strings.Cut(s, "="); return a }
func boolToInt(b bool) int {
	if b {
		return 1
	}
	return 0
}

// ---------------------------------------------------------------------------
// unicode + unicode/utf8.
// ---------------------------------------------------------------------------

func runStdUnicode() {
	section("std-unicode")

	chkTrue("unicode/IsLetter", unicode.IsLetter('A'))
	chk("unicode/IsLetter-false", boolToInt(unicode.IsLetter('1')), 0)
	chkTrue("unicode/IsDigit", unicode.IsDigit('7'))
	chkTrue("unicode/IsSpace", unicode.IsSpace(' '))
	chkTrue("unicode/IsUpper", unicode.IsUpper('A'))
	chkTrue("unicode/IsLower", unicode.IsLower('a'))
	chkTrue("unicode/IsPunct", unicode.IsPunct('!'))
	chk("unicode/ToUpper", int(unicode.ToUpper('a')), int('A'))
	chk("unicode/ToLower", int(unicode.ToLower('A')), int('a'))
	chkTrue("unicode/Is-Han", unicode.Is(unicode.Han, '世'))

	// utf8.
	chk("utf8/RuneLen-ascii", utf8.RuneLen('A'), 1)
	chk("utf8/RuneLen-cjk", utf8.RuneLen('世'), 3)
	chk("utf8/RuneCountInString", utf8.RuneCountInString("héllo"), 5)
	chkTrue("utf8/ValidString", utf8.ValidString("héllo"))
	chk("utf8/Valid-bad", boolToInt(utf8.Valid([]byte{0xff, 0xfe})), 0)
	r, size := utf8.DecodeRuneInString("世界")
	chk("utf8/DecodeRune", int(r), int('世'))
	chk("utf8/DecodeRune-size", size, 3)
	buf := make([]byte, 4)
	en := utf8.EncodeRune(buf, '世')
	chk("utf8/EncodeRune-n", en, 3)
	chkStr("utf8/EncodeRune", string(buf[:en]), "世")
}
