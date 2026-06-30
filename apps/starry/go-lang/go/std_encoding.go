package main

import (
	"bufio"
	"bytes"
	"compress/gzip"
	"encoding/base64"
	"encoding/binary"
	"encoding/csv"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"strings"
)

// ---------------------------------------------------------------------------
// encoding/json, base64, hex, csv, binary.
// ---------------------------------------------------------------------------

type person struct {
	Name    string   `json:"name"`
	Age     int      `json:"age"`
	Email   string   `json:"email,omitempty"`
	Hobbies []string `json:"hobbies"`
	private int      // unexported -> not serialized
}

func runStdEncoding() {
	section("std-encoding")

	// JSON Marshal: struct -> stable JSON (field order follows declaration).
	p := person{Name: "leo", Age: 30, Hobbies: []string{"go", "os"}, private: 99}
	b, _ := json.Marshal(p)
	chkStr("json/Marshal", string(b), `{"name":"leo","age":30,"hobbies":["go","os"]}`)

	// JSON Unmarshal back into a struct.
	var q person
	_ = json.Unmarshal([]byte(`{"name":"ann","age":25,"email":"a@x","hobbies":["read"]}`), &q)
	chkStr("json/Unmarshal-name", q.Name, "ann")
	chk("json/Unmarshal-age", q.Age, 25)
	chkStr("json/Unmarshal-email", q.Email, "a@x")
	chkStr("json/Unmarshal-hobby", q.Hobbies[0], "read")

	// MarshalIndent produces stable pretty output.
	bi, _ := json.MarshalIndent(struct {
		A int `json:"a"`
	}{A: 1}, "", "  ")
	chkStr("json/MarshalIndent", string(bi), "{\n  \"a\": 1\n}")

	// Marshal a map (json sorts map keys).
	mb, _ := json.Marshal(map[string]int{"z": 26, "a": 1, "m": 13})
	chkStr("json/Marshal-map-sorted", string(mb), `{"a":1,"m":13,"z":26}`)

	// Unmarshal into map[string]any (numbers -> float64).
	var generic map[string]any
	_ = json.Unmarshal([]byte(`{"n":42,"s":"hi","b":true,"arr":[1,2]}`), &generic)
	chk("json/generic-number", generic["n"].(float64), 42.0)
	chkStr("json/generic-string", generic["s"].(string), "hi")
	chkTrue("json/generic-bool", generic["b"].(bool))
	chk("json/generic-arr-len", len(generic["arr"].([]any)), 2)

	// json.Valid.
	chkTrue("json/Valid", json.Valid([]byte(`{"ok":true}`)))
	chk("json/Valid-bad", boolToInt(json.Valid([]byte(`{bad}`))), 0)

	// Streaming Encoder/Decoder.
	var enc bytes.Buffer
	e := json.NewEncoder(&enc)
	_ = e.Encode(map[string]int{"x": 1})
	chkStr("json/Encoder", strings.TrimSpace(enc.String()), `{"x":1}`)
	var decoded map[string]int
	_ = json.NewDecoder(strings.NewReader(`{"y":2}`)).Decode(&decoded)
	chk("json/Decoder", decoded["y"], 2)

	// base64 std + url.
	enc64 := base64.StdEncoding.EncodeToString([]byte("hello"))
	chkStr("base64/Std-encode", enc64, "aGVsbG8=")
	dec64, _ := base64.StdEncoding.DecodeString("aGVsbG8=")
	chkStr("base64/Std-decode", string(dec64), "hello")
	chkStr("base64/URL-encode", base64.URLEncoding.EncodeToString([]byte{0xfb, 0xff}), "-_8=")
	chkStr("base64/RawStd", base64.RawStdEncoding.EncodeToString([]byte("hi")), "aGk")

	// hex.
	chkStr("hex/EncodeToString", hex.EncodeToString([]byte{0xde, 0xad, 0xbe, 0xef}), "deadbeef")
	hb, _ := hex.DecodeString("cafe")
	chkStr("hex/DecodeString", fmt.Sprint(hb), "[202 254]")

	// csv read.
	r := csv.NewReader(strings.NewReader("a,b,c\n1,2,3\n"))
	rows, _ := r.ReadAll()
	chk("csv/rows", len(rows), 2)
	chkStr("csv/cell", rows[1][2], "3")
	// csv write.
	var cw bytes.Buffer
	w := csv.NewWriter(&cw)
	_ = w.Write([]string{"x", "y"})
	_ = w.Write([]string{"1", "2"})
	w.Flush()
	chkStr("csv/Write", cw.String(), "x,y\n1,2\n")

	// encoding/binary: BigEndian / LittleEndian round-trips.
	var be [4]byte
	binary.BigEndian.PutUint32(be[:], 0x01020304)
	chkStr("binary/BigEndian-bytes", fmt.Sprint(be), "[1 2 3 4]")
	chk("binary/BigEndian-read", int(binary.BigEndian.Uint32(be[:])), 0x01020304)
	var le [4]byte
	binary.LittleEndian.PutUint32(le[:], 0x01020304)
	chkStr("binary/LittleEndian-bytes", fmt.Sprint(le), "[4 3 2 1]")
	chk("binary/LittleEndian-read", int(binary.LittleEndian.Uint32(le[:])), 0x01020304)
	// Varint.
	var vb [binary.MaxVarintLen64]byte
	vn := binary.PutUvarint(vb[:], 300)
	val, _ := binary.Uvarint(vb[:vn])
	chk("binary/Uvarint", int(val), 300)
}

// ---------------------------------------------------------------------------
// io + bufio.
// ---------------------------------------------------------------------------

func runStdIOBufio() {
	section("std-io-bufio")

	// io.Copy from a reader to a buffer.
	var dst bytes.Buffer
	n, _ := io.Copy(&dst, strings.NewReader("copy me"))
	chk("io/Copy-n", int(n), 7)
	chkStr("io/Copy-content", dst.String(), "copy me")

	// io.ReadAll.
	all, _ := io.ReadAll(strings.NewReader("read all of this"))
	chkStr("io/ReadAll", string(all), "read all of this")

	// io.WriteString.
	var ws bytes.Buffer
	wn, _ := io.WriteString(&ws, "abc")
	chk("io/WriteString", wn, 3)

	// io.MultiReader concatenates readers.
	mr := io.MultiReader(strings.NewReader("foo"), strings.NewReader("bar"))
	mrAll, _ := io.ReadAll(mr)
	chkStr("io/MultiReader", string(mrAll), "foobar")

	// io.LimitReader caps bytes.
	lr := io.LimitReader(strings.NewReader("0123456789"), 4)
	lrAll, _ := io.ReadAll(lr)
	chkStr("io/LimitReader", string(lrAll), "0123")

	// io.TeeReader duplicates reads into a side buffer.
	var tee bytes.Buffer
	tr := io.TeeReader(strings.NewReader("teed"), &tee)
	_, _ = io.ReadAll(tr)
	chkStr("io/TeeReader", tee.String(), "teed")

	// bufio.Scanner: line and word splitting.
	sc := bufio.NewScanner(strings.NewReader("l1\nl2\nl3"))
	var lines []string
	for sc.Scan() {
		lines = append(lines, sc.Text())
	}
	chkStr("bufio/Scanner-lines", strings.Join(lines, "|"), "l1|l2|l3")

	wsc := bufio.NewScanner(strings.NewReader("the quick brown"))
	wsc.Split(bufio.ScanWords)
	count := 0
	for wsc.Scan() {
		count++
	}
	chk("bufio/Scanner-words", count, 3)

	// bufio.Reader: ReadString delimiter.
	br := bufio.NewReader(strings.NewReader("a,b,c"))
	tok, _ := br.ReadString(',')
	chkStr("bufio/ReadString", tok, "a,")

	// bufio.Writer buffers then flushes.
	var bw bytes.Buffer
	w := bufio.NewWriter(&bw)
	_, _ = w.WriteString("buffered")
	chk("bufio/Writer-before-flush", bw.Len(), 0) // still buffered
	_ = w.Flush()
	chkStr("bufio/Writer-after-flush", bw.String(), "buffered")
}

// ---------------------------------------------------------------------------
// compress/gzip (round-trip on a deterministic payload).
// ---------------------------------------------------------------------------

func gzipRoundTrip(s string) string {
	var buf bytes.Buffer
	zw := gzip.NewWriter(&buf)
	_, _ = zw.Write([]byte(s))
	_ = zw.Close()
	zr, _ := gzip.NewReader(&buf)
	out, _ := io.ReadAll(zr)
	_ = zr.Close()
	return string(out)
}
