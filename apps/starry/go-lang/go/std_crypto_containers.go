package main

import (
	"container/heap"
	"container/list"
	"container/ring"
	"crypto/hmac"
	"crypto/md5"
	"crypto/sha256"
	"crypto/sha512"
	"encoding/hex"
	"fmt"
	"hash/crc32"
	"hash/fnv"
)

// ---------------------------------------------------------------------------
// hashing + crypto digests (deterministic: fixed input -> fixed digest).
// ---------------------------------------------------------------------------

func runStdHashCrypto() {
	section("std-hash-crypto")

	// crypto/sha256 — known vector for "abc".
	sum := sha256.Sum256([]byte("abc"))
	chkStr("sha256/abc", hex.EncodeToString(sum[:]),
		"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
	// Empty string vector.
	empty := sha256.Sum256(nil)
	chkStr("sha256/empty", hex.EncodeToString(empty[:]),
		"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")

	// crypto/sha512.
	s512 := sha512.Sum512([]byte("abc"))
	chkStr("sha512/abc-prefix", hex.EncodeToString(s512[:])[:16], "ddaf35a193617aba")

	// crypto/md5 — known vector for "abc" (digest use only, not security).
	m := md5.Sum([]byte("abc"))
	chkStr("md5/abc", hex.EncodeToString(m[:]), "900150983cd24fb0d6963f7d28e17f72")

	// crypto/hmac — deterministic for fixed key+message.
	h := hmac.New(sha256.New, []byte("key"))
	h.Write([]byte("The quick brown fox jumps over the lazy dog"))
	chkStr("hmac/sha256",
		hex.EncodeToString(h.Sum(nil)),
		"f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8")
	// hmac.Equal constant-time compare.
	a := hmac.New(sha256.New, []byte("k"))
	a.Write([]byte("x"))
	b := hmac.New(sha256.New, []byte("k"))
	b.Write([]byte("x"))
	chkTrue("hmac/Equal", hmac.Equal(a.Sum(nil), b.Sum(nil)))

	// hash/fnv.
	f := fnv.New32a()
	f.Write([]byte("hello"))
	chk("fnv/32a", int(f.Sum32()), 1335831723)
	f64 := fnv.New64a()
	f64.Write([]byte("hello"))
	chkStr("fnv/64a", fmt.Sprintf("%x", f64.Sum64()), "a430d84680aabd0b")

	// hash/crc32.
	chk("crc32/IEEE", int(crc32.ChecksumIEEE([]byte("hello"))), 907060870)
	tab := crc32.MakeTable(crc32.Castagnoli)
	chk("crc32/Castagnoli", int(crc32.Checksum([]byte("hello"), tab)), 2591144780)

	// Incremental write equivalence: streaming == one-shot.
	hs := sha256.New()
	hs.Write([]byte("ab"))
	hs.Write([]byte("c"))
	chkStr("sha256/streaming-eq", hex.EncodeToString(hs.Sum(nil)), hex.EncodeToString(sum[:]))

	// Cross-check the gzip round-trip helper from std_encoding.go here.
	chkStr("gzip/roundtrip", gzipRoundTrip("compress me, please, twice over"),
		"compress me, please, twice over")
}

// ---------------------------------------------------------------------------
// container/list, container/heap, container/ring.
// ---------------------------------------------------------------------------

// intHeap is a min-heap of ints implementing heap.Interface.
type intHeap []int

func (h intHeap) Len() int            { return len(h) }
func (h intHeap) Less(i, j int) bool  { return h[i] < h[j] }
func (h intHeap) Swap(i, j int)       { h[i], h[j] = h[j], h[i] }
func (h *intHeap) Push(x any)         { *h = append(*h, x.(int)) }
func (h *intHeap) Pop() any {
	old := *h
	n := len(old)
	v := old[n-1]
	*h = old[:n-1]
	return v
}

func runStdContainers() {
	section("std-containers")

	// container/list (doubly linked list).
	l := list.New()
	l.PushBack(1)
	l.PushBack(2)
	l.PushFront(0)
	chk("list/Len", l.Len(), 3)
	chk("list/Front", l.Front().Value.(int), 0)
	chk("list/Back", l.Back().Value.(int), 2)
	// Forward traversal sum.
	sum := 0
	for e := l.Front(); e != nil; e = e.Next() {
		sum += e.Value.(int)
	}
	chk("list/traverse-sum", sum, 3)
	// Remove an element.
	l.Remove(l.Front())
	chk("list/Remove", l.Front().Value.(int), 1)

	// container/heap (priority queue): popping yields sorted order.
	h := &intHeap{5, 2, 8, 1, 9, 3}
	heap.Init(h)
	heap.Push(h, 0)
	var popped []int
	for h.Len() > 0 {
		popped = append(popped, heap.Pop(h).(int))
	}
	chkStr("heap/sorted-pop", fmt.Sprint(popped), "[0 1 2 3 5 8 9]")
	chk("heap/min-first", popped[0], 0)

	// container/ring (circular list).
	r := ring.New(5)
	for i := 0; i < r.Len(); i++ {
		r.Value = i
		r = r.Next()
	}
	rsum := 0
	r.Do(func(v any) { rsum += v.(int) })
	chk("ring/Len", r.Len(), 5)
	chk("ring/sum", rsum, 10) // 0+1+2+3+4
	// Move forward 2 and read.
	chk("ring/Move", r.Move(2).Value.(int), 2)
}
