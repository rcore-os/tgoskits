package main

import (
	"archive/tar"
	"archive/zip"
	"bytes"
	"compress/flate"
	"compress/gzip"
	"compress/zlib"
	"crypto/aes"
	"crypto/cipher"
	"crypto/ed25519"
	"crypto/sha1"
	"encoding/ascii85"
	"encoding/base32"
	"encoding/gob"
	"encoding/pem"
	"flag"
	"fmt"
	"hash/adler32"
	"hash/crc64"
	"io"
	"io/fs"
	"log"
	"net/url"
	"regexp/syntax"
	"strings"
	"testing/fstest"
	"text/scanner"
	"text/tabwriter"
)

func runStdMore() {
	section("stdlib-more")

	// --- archive/tar ---
	chk("tar/header", (func() bool {
		var buf bytes.Buffer
		tw := tar.NewWriter(&buf)
		hdr := &tar.Header{Name: "f.txt", Size: 6, Mode: 0644}
		chkTrue("tar/write-header", tw.WriteHeader(hdr) == nil)
		chkTrue("tar/write-body", (func() bool { _, e := tw.Write([]byte("hello\n")); return e == nil })())
		tw.Close()
		tr := tar.NewReader(&buf)
		h, _ := tr.Next()
		return h.Name == "f.txt" && h.Size == 6
	})(), true)

	// --- archive/zip ---
	chk("zip/roundtrip", (func() bool {
		var buf bytes.Buffer
		zw := zip.NewWriter(&buf)
		w, _ := zw.Create("a.txt")
		w.Write([]byte("zipdata"))
		zw.Close()
		zr, _ := zip.NewReader(bytes.NewReader(buf.Bytes()), int64(buf.Len()))
		return len(zr.File) == 1 && zr.File[0].Name == "a.txt"
	})(), true)

	// --- compress/gzip ---
	chk("gzip/roundtrip", (func() bool {
		var buf bytes.Buffer
		gw := gzip.NewWriter(&buf)
		gw.Write([]byte("gzdata"))
		gw.Close()
		gr, _ := gzip.NewReader(&buf)
		out, _ := io.ReadAll(gr)
		return string(out) == "gzdata"
	})(), true)

	// --- compress/zlib ---
	chk("zlib/roundtrip", (func() bool {
		var buf bytes.Buffer
		zw := zlib.NewWriter(&buf)
		zw.Write([]byte("zlibdata"))
		zw.Close()
		zr, _ := zlib.NewReader(&buf)
		out, _ := io.ReadAll(zr)
		return string(out) == "zlibdata"
	})(), true)

	// --- compress/flate ---
	chk("flate/roundtrip", (func() bool {
		var buf bytes.Buffer
		fw, _ := flate.NewWriter(&buf, flate.DefaultCompression)
		fw.Write([]byte("flatedata"))
		fw.Close()
		fr := flate.NewReader(&buf)
		out, _ := io.ReadAll(fr)
		return string(out) == "flatedata"
	})(), true)

	// --- crypto/aes + cipher (AES-GCM fixed key, deterministic) ---
	key := make([]byte, 32) // all zeros
	plain := []byte("aes-gcm-test-msg")
	block, _ := aes.NewCipher(key)
	aesgcm, _ := cipher.NewGCM(block)
	nonce := make([]byte, aesgcm.NonceSize())
	ciphertext := aesgcm.Seal(nil, nonce, plain, nil)
	decrypted, _ := aesgcm.Open(nil, nonce, ciphertext, nil)
	chkStr("aes/gcm-decrypt", string(decrypted), "aes-gcm-test-msg")

	// --- crypto/ed25519 ---
	pub, priv, _ := ed25519.GenerateKey(nil)
	sig := ed25519.Sign(priv, []byte("msg"))
	chkTrue("ed25519/verify", ed25519.Verify(pub, []byte("msg"), sig))
	chkTrue("ed25519/verify-wrong-msg", !ed25519.Verify(pub, []byte("wrong"), sig))

	// --- crypto/sha1 ---
	h := sha1.New()
	h.Write([]byte("sha1-test"))
	chkStr("sha1/sum", fmt.Sprintf("%x", h.Sum(nil)), "cebb8a6019488e80ca1e1c92322cfdfbff5c04a4")

	// --- hash/crc64 ---
	crcTable := crc64.MakeTable(crc64.ECMA)
	c := crc64.New(crcTable)
	c.Write([]byte("crc64"))
	chkTrue("crc64/checksum", c.Sum64() == crc64.Checksum([]byte("crc64"), crcTable))

	// --- hash/adler32 ---
	chkTrue("adler32/checksum", adler32.Checksum([]byte("adler")) != 0)

	// --- encoding/base32 ---
	enc := base32.StdEncoding.EncodeToString([]byte("base32"))
	dec, _ := base32.StdEncoding.DecodeString(enc)
	chkStr("base32/roundtrip", string(dec), "base32")

	// --- encoding/pem ---
	pemBlock := &pem.Block{Type: "TEST", Bytes: []byte("pemdata")}
	pemEncoded := pem.EncodeToMemory(pemBlock)
	pemDecoded, _ := pem.Decode(pemEncoded)
	chkStr("pem/roundtrip-type", pemDecoded.Type, "TEST")
	chkStr("pem/roundtrip-data", string(pemDecoded.Bytes), "pemdata")

	// --- encoding/ascii85 ---
	var a85buf bytes.Buffer
	w85 := ascii85.NewEncoder(&a85buf)
	w85.Write([]byte("a85"))
	w85.Close()
	r85 := ascii85.NewDecoder(strings.NewReader(a85buf.String()))
	out85, _ := io.ReadAll(r85)
	chkStr("ascii85/roundtrip", string(out85), "a85")

	// --- encoding/gob ---
	type gobStruct struct{ X, Y int }
	var gobBuf bytes.Buffer
	gob.NewEncoder(&gobBuf).Encode(gobStruct{1, 2})
	var decGob gobStruct
	gob.NewDecoder(&gobBuf).Decode(&decGob)
	chkTrue("gob/roundtrip-X", decGob.X == 1)
	chkTrue("gob/roundtrip-Y", decGob.Y == 2)

	// --- net/url ---
	u, _ := url.Parse("https://example.com:8443/a?k=v#f")
	chkStr("url/scheme", u.Scheme, "https")
	chkStr("url/hostname", u.Hostname(), "example.com")
	chkStr("url/port", u.Port(), "8443")
	chkStr("url/path", u.Path, "/a")
	chkStr("url/query-encoded", u.RawQuery, "k=v")
	chkStr("url/fragment", u.Fragment, "f")
	// Query().Get
	chkStr("url/query-get", u.Query().Get("k"), "v")
	// url.Values encode
	v := url.Values{"a": {"1", "2"}}
	chkStr("url/values-encode", v.Encode(), "a=1&a=2")

	// --- flag ---
	flagSet := flag.NewFlagSet("test", flag.ContinueOnError)
	name := flagSet.String("name", "default", "name flag")
	flagSet.Parse([]string{"-name", "go"})
	chkStr("flag/parse", *name, "go")

	// --- log ---
	var logBuf bytes.Buffer
	log.SetOutput(&logBuf)
	log.Print("hi")
	chkTrue("log/print", strings.Contains(logBuf.String(), "hi"))
	log.SetOutput(io.Discard) // restore

	// --- io/fs (fstest.MapFS) ---
	mapFS := fstest.MapFS{"hello.txt": {Data: []byte("world")}}
	data, _ := fs.ReadFile(mapFS, "hello.txt")
	chkStr("fstest/mapfs-read", string(data), "world")

	// --- text/scanner ---
	var s scanner.Scanner
	s.Init(strings.NewReader("123 xyz"))
	chkTrue("scanner/scan-int", s.Scan() == scanner.Int)
	chkStr("scanner/token-text", s.TokenText(), "123")
	// text/scanner skips whitespace automatically, so the next Scan returns the
	// identifier directly (there is no separate whitespace token to consume).
	chkTrue("scanner/scan-ident", s.Scan() == scanner.Ident)
	chkStr("scanner/token-ident", s.TokenText(), "xyz")

	// --- text/tabwriter ---
	var twBuf bytes.Buffer
	tw := tabwriter.NewWriter(&twBuf, 0, 8, 1, '\t', 0)
	fmt.Fprintf(tw, "a\tb\tc\n1\t2\t3\n")
	tw.Flush()
	chkTrue("tabwriter/output", len(twBuf.String()) > 0)

	// --- regexp/syntax ---
	re, _ := syntax.Parse("a(b|c)*d", syntax.Perl)
	chkTrue("regexp-syntax/parse", re != nil)
	chkTrue("regexp-syntax/op", re.Op == syntax.OpConcat)

}
