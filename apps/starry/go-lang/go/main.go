// golang-lang — a COMPREHENSIVE, doc-grounded Go 1.26 LANGUAGE carpet for the
// StarryOS #764 "other: golang <!-- 1.26 -->" on-target language-layer test.
//
// It is the Go analogue of the python-lang suite: every section drives a slice
// of the language spec (go.dev/ref/spec), the standard-library index
// (pkg.go.dev/std), or the go1.26 release notes (go.dev/doc/go1.26), and asserts
// EXACT expected values. Every line of output is DETERMINISTIC: no wall-clock,
// no map-iteration order, no goroutine-scheduling order, no pointer addresses.
// Sums are order-independent, map/set reads go through sorted keys, time values
// are fixed instants. The binary is built with CGO_ENABLED=0 so it is a fully
// static ELF that runs on musl/StarryOS with no libc.
//
// Output contract (mirrors python-lang run_all gating):
//   - each assertion prints one stable "ok: <label> = <value>" line via chk();
//   - on the very first mismatch the program prints "FAIL: ..." and exits 1
//     (so a regression can never be masked by a later pass);
//   - on full success it prints "GOLANG count=<N>" then "GO_LANG_OK".
//
// success_regex anchor: (?m)^GO_LANG_OK$ ; fail anchor: (?i)\b(FAIL|panic)\b.
package main

import (
	"fmt"
	"os"
)

// checks counts every assertion that has passed; it becomes the count=N total.
var checks int

// failed is set the moment any assertion mismatches; main() refuses the OK token.
var failed bool

// chk asserts that got == want, printing one deterministic line per assertion.
// The value is rendered with %v through a canonical path so the golden file is
// stable. On mismatch it prints FAIL (matched by the fail_regex) and the suite
// aborts via main's gate so a later pass cannot mask the regression.
func chk[T comparable](label string, got, want T) {
	if got != want {
		failed = true
		fmt.Printf("FAIL: %s: got %v want %v\n", label, got, want)
		// Abort immediately: a deterministic suite must not continue past a
		// real divergence (a later section could otherwise "pass" and hide it).
		fmt.Printf("GOLANG count=%d\n", checks)
		os.Exit(1)
	}
	checks++
	fmt.Printf("ok: %s = %v\n", label, got)
}

// chkStr is chk specialised to strings, used pervasively so callers can pass a
// formatted summary string (e.g. a sorted-key dump) as the single comparable.
func chkStr(label, got, want string) { chk(label, got, want) }

// chkTrue asserts a boolean predicate is true (the common "this happened" case).
func chkTrue(label string, got bool) { chk(label, got, true) }

// fwOK records one framework-section observation as a passed assertion. The
// framework carpets (gin/grpc/go-zero/gorm/sqlite) are exercised through real
// in-memory harnesses (httptest / bufconn / in-mem SQLite) whose observed values
// are fully deterministic; the *value itself* is the assertion — it is captured
// in the golden and compared byte-for-byte on-target, so a regression in any
// framework path changes the line and fails the cmp gate (exactly the carpet's
// determinism contract). fwOK shares the global `checks` counter so the framework
// assertions roll into the single GO_LANG_OK total alongside the language/stdlib
// sections, and prints the identical "ok: <label> = <value>" line shape.
func fwOK(label string, value any) {
	checks++
	fmt.Printf("ok: %s = %v\n", label, value)
}

// fwMust records an error-returning operation as a single assertion, rendering
// nil as "nil" and a non-nil error as "ERR(<msg>)" so the golden is stable and a
// regression that flips success/failure changes the line.
func fwMust(label string, err error) {
	checks++
	if err != nil {
		fmt.Printf("ok: %s = ERR(%v)\n", label, err)
	} else {
		fmt.Printf("ok: %s = nil\n", label)
	}
}

// section prints a banner so the golden file is readable and so a failing
// region is easy to locate. Banners are NOT counted as assertions.
func section(name string) { fmt.Printf("== %s ==\n", name) }

// intsToStr renders an []int the way fmt.Sprint does ("[1 2 3]"), as a stable
// comparable string for chkStr.
func intsToStr(xs []int) string { return fmt.Sprint(xs) }

func main() {
	// Language layer (spec-grounded).
	runBasicTypes()
	runConstantsIota()
	runOperators()
	runStringsRunesBytes()
	runArraysSlices()
	runMaps()
	runStructsEmbedding()
	runMethodsInterfaces()
	runTypeAssertSwitch()
	runGenerics()
	runClosuresVariadics()
	runDeferPanicRecover()
	runControlFlow()
	runRangeForms()
	runErrorsLanguage()

	// Concurrency / async layer (deterministic).
	runGoroutinesChannels()
	runSelect()
	runSyncPrimitives()
	runAtomics()
	runContext()
	runConcurrencyPatterns()

	// Standard library layer (one section, many packages, real assertions).
	runStdFmtStrconv()
	runStdStringsBytes()
	runStdUnicode()
	runStdSortSlicesMapsCmp()
	runStdMath()
	runStdTime()
	runStdRegexp()
	runStdEncoding()
	runStdIOBufio()
	runStdHashCrypto()
	runStdContainers()
	runStdTemplates()
	runStdReflect()
	runStdMisc() // os/path/filepath/netip/slog/iter/binary/gzip/utf16/structs/...
	runStdMore() // archive(tar/zip) · compress(gzip/zlib/flate) · crypto(aes-gcm/ed25519/sha1) · hash(crc64/adler32) · encoding(base32/pem/ascii85/gob) · net/url · flag · log · io/fs · text(scanner/tabwriter) · regexp/syntax

	// Go 1.26 specifics (release-notes-grounded).
	runGo126Features()

	// Framework layer (pure-Go, deterministic in-memory harnesses; folded into
	// the same static CGO_ENABLED=0 binary). Each section drives a real ecosystem
	// framework through httptest / bufconn / in-memory SQLite — no socket, no
	// wall-clock, no map-iteration order — and its observed values are asserted
	// byte-for-byte against the golden via fwOK/fwMust.
	runFrameworkGin()    // github.com/gin-gonic/gin (httptest)
	runFrameworkGRPC()   // google.golang.org/grpc (bufconn)
	runFrameworkGoZero() // github.com/zeromicro/go-zero (rest httptest + zrpc bufconn + core)
	runFrameworkSQLite() // database/sql + pure-Go modernc driver
	runFrameworkGORM()   // gorm.io/gorm + glebarez/sqlite (in-mem)

	// Gate: only emit the OK token when every assertion passed.
	if failed {
		fmt.Println("SUITE FAILED")
		os.Exit(1)
	}
	fmt.Printf("GOLANG count=%d\n", checks)
	fmt.Println("GO_LANG_OK")
}
