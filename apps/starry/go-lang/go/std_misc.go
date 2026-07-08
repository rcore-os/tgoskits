package main

import (
	"bytes"
	"fmt"
	"iter"
	"log/slog"
	"math/cmplx"
	"net/netip"
	"os"
	"path"
	"path/filepath"
	"slices"
	"strings"
	"unicode/utf16"
	"unique"
)

// ---------------------------------------------------------------------------
// os (env/args-safe), path, path/filepath, net/netip, slog, iter, cmplx,
// unicode/utf16, unique. No wall clock, no real FS mutation in a way that
// would be non-deterministic — we use Setenv on private keys we control.
// ---------------------------------------------------------------------------

func runStdMisc() {
	section("std-misc")

	// os: deterministic env operations on a key we own.
	_ = os.Setenv("GOLANGLANG_TEST_KEY", "v123")
	chkStr("os/Getenv", os.Getenv("GOLANGLANG_TEST_KEY"), "v123")
	val, ok := os.LookupEnv("GOLANGLANG_TEST_KEY")
	chkStr("os/LookupEnv-value", val, "v123")
	chkTrue("os/LookupEnv-ok", ok)
	_, miss := os.LookupEnv("GOLANGLANG_DEFINITELY_MISSING_KEY")
	chk("os/LookupEnv-miss", boolToInt(miss), 0)
	_ = os.Unsetenv("GOLANGLANG_TEST_KEY")
	chkStr("os/Unsetenv", os.Getenv("GOLANGLANG_TEST_KEY"), "")
	// os.Args is non-empty (program name); assert structurally, not by content.
	chkTrue("os/Args-nonempty", len(os.Args) >= 1)
	// os.Expand / os.ExpandEnv with a controlled mapping.
	expanded := os.Expand("$A-${B}", func(k string) string {
		return map[string]string{"A": "1", "B": "2"}[k]
	})
	chkStr("os/Expand", expanded, "1-2")

	// path (always forward-slash, URL-like).
	chkStr("path/Join", path.Join("a", "b", "c"), "a/b/c")
	chkStr("path/Join-clean", path.Join("a/", "/b/", "../c"), "a/c")
	chkStr("path/Base", path.Base("/x/y/z.txt"), "z.txt")
	chkStr("path/Dir", path.Dir("/x/y/z.txt"), "/x/y")
	chkStr("path/Ext", path.Ext("file.tar.gz"), ".gz")
	chkStr("path/Clean", path.Clean("a//b/../c/./d"), "a/c/d")
	chkTrue("path/IsAbs", path.IsAbs("/abs"))
	pdir, pfile := path.Split("/a/b/c.go")
	chkStr("path/Split-dir", pdir, "/a/b/")
	chkStr("path/Split-file", pfile, "c.go")

	// path/filepath (OS-aware; on Linux uses "/").
	chkStr("filepath/Join", filepath.Join("a", "b", "c"), "a/b/c")
	chkStr("filepath/Base", filepath.Base("/x/y/z.txt"), "z.txt")
	chkStr("filepath/Dir", filepath.Dir("/x/y/z.txt"), "/x/y")
	chkStr("filepath/Ext", filepath.Ext("a.go"), ".go")
	chkStr("filepath/Clean", filepath.Clean("a//b/../c"), "a/c")
	chkTrue("filepath/IsAbs", filepath.IsAbs("/root"))
	rel, _ := filepath.Rel("/a/b", "/a/b/c/d")
	chkStr("filepath/Rel", rel, "c/d")
	chkTrue("filepath/Match", mustMatch("*.go", "main.go"))
	chk("filepath/Match-false", boolToInt(mustMatch("*.go", "main.py")), 0)
	chkStr("filepath/VolumeName-empty", filepath.VolumeName("/a/b"), "") // empty on unix
	chkStr("filepath/ToSlash", filepath.ToSlash("a/b"), "a/b")

	// net/netip: parse, classify, compare, contains.
	a4 := netip.MustParseAddr("10.0.0.1")
	chkStr("netip/Addr-string", a4.String(), "10.0.0.1")
	chkTrue("netip/Is4", a4.Is4())
	chk("netip/Is6", boolToInt(a4.Is6()), 0)
	chk("netip/BitLen", a4.BitLen(), 32)
	chkTrue("netip/IsPrivate", a4.IsPrivate())
	chkStr("netip/Next", a4.Next().String(), "10.0.0.2")
	chkStr("netip/Prev", a4.Prev().String(), "10.0.0.0")
	chkTrue("netip/loopback", netip.MustParseAddr("127.0.0.1").IsLoopback())
	a6 := netip.MustParseAddr("2001:db8::1")
	chkTrue("netip/Is6-v6", a6.Is6())
	chk("netip/BitLen-v6", a6.BitLen(), 128)
	// As4 returns the 4-byte form.
	chkStr("netip/As4", fmt.Sprint(a4.As4()), "[10 0 0 1]")
	// Prefix: contains + masked + bits + compare.
	pfx := netip.MustParsePrefix("10.0.0.0/24")
	chkTrue("netip/Prefix-contains", pfx.Contains(netip.MustParseAddr("10.0.0.5")))
	chk("netip/Prefix-contains-false", boolToInt(pfx.Contains(netip.MustParseAddr("10.0.1.5"))), 0)
	chk("netip/Prefix-bits", pfx.Bits(), 24)
	chkStr("netip/Prefix-masked", netip.MustParsePrefix("10.0.0.55/24").Masked().String(), "10.0.0.0/24")
	chkTrue("netip/Prefix-overlaps", netip.MustParsePrefix("10.0.0.0/16").Overlaps(netip.MustParsePrefix("10.0.0.0/24")))
	// AddrPort.
	ap := netip.AddrPortFrom(a4, 8080)
	chkStr("netip/AddrPort", ap.String(), "10.0.0.1:8080")
	chk("netip/AddrPort-port", int(ap.Port()), 8080)
	apv6 := netip.AddrPortFrom(a6, 443)
	chkStr("netip/AddrPort-v6", apv6.String(), "[2001:db8::1]:443")
	// Comparable: usable as map key / ==.
	chkTrue("netip/Addr-equal", netip.MustParseAddr("1.2.3.4") == netip.MustParseAddr("1.2.3.4"))

	// log/slog: structured logging to a buffer with a fixed handler.
	var lb bytes.Buffer
	logger := slog.New(slog.NewTextHandler(&lb, &slog.HandlerOptions{
		Level: slog.LevelInfo,
		// Strip the time attribute so output is deterministic.
		ReplaceAttr: func(groups []string, a slog.Attr) slog.Attr {
			if a.Key == slog.TimeKey {
				return slog.Attr{}
			}
			return a
		},
	}))
	logger.Info("started", "service", "go", "port", 8080)
	out := lb.String()
	chkTrue("slog/text-has-msg", strings.Contains(out, "msg=started"))
	chkTrue("slog/text-has-attr", strings.Contains(out, "service=go"))
	chkTrue("slog/text-has-level", strings.Contains(out, "level=INFO"))
	chk("slog/text-no-time", boolToInt(strings.Contains(out, "time=")), 0)
	// Debug is below Info threshold -> suppressed.
	lb.Reset()
	logger.Debug("hidden")
	chk("slog/level-filter", lb.Len(), 0)
	// JSON handler.
	var jb bytes.Buffer
	jlog := slog.New(slog.NewJSONHandler(&jb, &slog.HandlerOptions{
		ReplaceAttr: func(groups []string, a slog.Attr) slog.Attr {
			if a.Key == slog.TimeKey {
				return slog.Attr{}
			}
			return a
		},
	}))
	jlog.Info("evt", "n", 1)
	chkTrue("slog/json-has-msg", strings.Contains(jb.String(), `"msg":"evt"`))
	chkTrue("slog/json-has-attr", strings.Contains(jb.String(), `"n":1`))
	// With + groups.
	lb.Reset()
	logger.With("req", "abc").Info("handled")
	chkTrue("slog/with-attr", strings.Contains(lb.String(), "req=abc"))

	// iter: building & consuming custom iterators (Seq filtering).
	evens := func(yield func(int) bool) {
		for i := 0; i < 10; i++ {
			if i%2 == 0 && !yield(i) {
				return
			}
		}
	}
	chkStr("iter/Seq-collect", fmt.Sprint(slices.Collect(iter.Seq[int](evens))), "[0 2 4 6 8]")

	// math/cmplx.
	z := complex(0, 1) // i
	chk("cmplx/Abs", cmplx.Abs(complex(3, 4)), 5.0)
	chkStr("cmplx/Conj", fmt.Sprint(cmplx.Conj(complex(2, 3))), "(2-3i)")
	// i^2 = -1 (real part); use Pow.
	sq := z * z
	chk("cmplx/i-squared-real", real(sq), -1.0)
	chk("cmplx/Phase-i", int(cmplx.Phase(z)*1000), 1570) // ~pi/2

	// unicode/utf16 encode/decode round-trip including a surrogate pair.
	runes := []rune("A𝄞") // 𝄞 (U+1D11E) needs a surrogate pair
	u16 := utf16.Encode(runes)
	chk("utf16/Encode-len", len(u16), 3) // 'A' + surrogate pair
	back := utf16.Decode(u16)
	chkStr("utf16/roundtrip", string(back), "A𝄞")
	r1, r2 := utf16.EncodeRune('𝄞')
	chkTrue("utf16/IsSurrogate", utf16.IsSurrogate(r1) && utf16.IsSurrogate(r2))
	chk("utf16/DecodeRune", int(utf16.DecodeRune(r1, r2)), 0x1D11E)

	// unique (Go 1.23): interned handles compare by value cheaply.
	h1 := unique.Make("interned")
	h2 := unique.Make("interned")
	chkTrue("unique/Make-equal", h1 == h2)
	chkStr("unique/Value", h1.Value(), "interned")
	h3 := unique.Make("other")
	chk("unique/Make-distinct", boolToInt(h1 == h3), 0)
}

func mustMatch(pattern, name string) bool {
	ok, _ := filepath.Match(pattern, name)
	return ok
}
