package main

import (
	"fmt"
	"sort"
	"sync"
	"sync/atomic"
)

// ---------------------------------------------------------------------------
// SYNC PRIMITIVES (pkg sync: Mutex, RWMutex, WaitGroup, Once, Map, Pool, Cond).
// All assertions are scheduling-independent (final counts/sums/membership).
// ---------------------------------------------------------------------------

func runSyncPrimitives() {
	section("sync-primitives")

	// Mutex: protect a shared counter incremented by many goroutines.
	var mu sync.Mutex
	shared := 0
	var wg sync.WaitGroup
	for i := 0; i < 100; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			mu.Lock()
			shared++
			mu.Unlock()
		}()
	}
	wg.Wait()
	chk("sync/mutex-count", shared, 100)

	// RWMutex: many readers + writers; final value deterministic.
	var rw sync.RWMutex
	data := 0
	var wg2 sync.WaitGroup
	for i := 0; i < 50; i++ {
		wg2.Add(1)
		go func() {
			defer wg2.Done()
			rw.Lock()
			data++
			rw.Unlock()
		}()
	}
	wg2.Wait()
	// Reader sees the final consistent value.
	rw.RLock()
	finalData := data
	rw.RUnlock()
	chk("sync/rwmutex-writes", finalData, 50)

	// WaitGroup already exercised; assert it gates completion (sum check).
	var wg3 sync.WaitGroup
	var total int64
	for i := 1; i <= 10; i++ {
		wg3.Add(1)
		go func(v int) {
			defer wg3.Done()
			atomic.AddInt64(&total, int64(v))
		}(i)
	}
	wg3.Wait()
	chk("sync/waitgroup-sum", int(total), 55)

	// Once: the function runs exactly once across many goroutines.
	var once sync.Once
	var onceCount int32
	var wg4 sync.WaitGroup
	for i := 0; i < 50; i++ {
		wg4.Add(1)
		go func() {
			defer wg4.Done()
			once.Do(func() { atomic.AddInt32(&onceCount, 1) })
		}()
	}
	wg4.Wait()
	chk("sync/once", int(atomic.LoadInt32(&onceCount)), 1)

	// sync.Map: concurrent map; read back via sorted keys.
	var sm sync.Map
	var wg5 sync.WaitGroup
	for i := 0; i < 10; i++ {
		wg5.Add(1)
		go func(k int) {
			defer wg5.Done()
			sm.Store(fmt.Sprintf("k%02d", k), k*k)
		}(i)
	}
	wg5.Wait()
	var keys []string
	var valSum int
	sm.Range(func(k, v any) bool {
		keys = append(keys, k.(string))
		valSum += v.(int)
		return true
	})
	sort.Strings(keys)
	chk("sync/map-count", len(keys), 10)
	chk("sync/map-valsum", valSum, 0+1+4+9+16+25+36+49+64+81) // 285
	chkStr("sync/map-first-key", keys[0], "k00")
	v, ok := sm.Load("k03")
	chkTrue("sync/map-load-ok", ok)
	chk("sync/map-load-val", v.(int), 9)
	// LoadOrStore on existing key returns existing value.
	actual, loaded := sm.LoadOrStore("k03", 999)
	chkTrue("sync/map-loadorstore-loaded", loaded)
	chk("sync/map-loadorstore-val", actual.(int), 9)

	// sync.Pool: get/put round-trip (we control determinism by always New).
	pool := sync.Pool{New: func() any { return new([16]byte) }}
	buf := pool.Get().(*[16]byte)
	buf[0] = 0xAB
	pool.Put(buf)
	chk("sync/pool-len", len(*buf), 16)
	// New() path gives a fresh zeroed buffer when pool is drained at construction.
	fresh := sync.Pool{New: func() any { return 7 }}
	chk("sync/pool-new", fresh.Get().(int), 7)

	// sync.Cond: producer signals, consumer waits for a condition.
	var cmu sync.Mutex
	cond := sync.NewCond(&cmu)
	ready := false
	result := 0
	go func() {
		cmu.Lock()
		ready = true
		result = 123
		cond.Signal()
		cmu.Unlock()
	}()
	cmu.Lock()
	for !ready {
		cond.Wait()
	}
	got := result
	cmu.Unlock()
	chk("sync/cond-signal", got, 123)

	// sync.Cond Broadcast wakes all waiters.
	var bmu sync.Mutex
	bcond := sync.NewCond(&bmu)
	open := false
	var awoke int32
	var wg6 sync.WaitGroup
	for i := 0; i < 5; i++ {
		wg6.Add(1)
		go func() {
			defer wg6.Done()
			bmu.Lock()
			for !open {
				bcond.Wait()
			}
			bmu.Unlock()
			atomic.AddInt32(&awoke, 1)
		}()
	}
	bmu.Lock()
	open = true
	bcond.Broadcast()
	bmu.Unlock()
	wg6.Wait()
	chk("sync/cond-broadcast", int(atomic.LoadInt32(&awoke)), 5)
}

// ---------------------------------------------------------------------------
// SYNC/ATOMIC (pkg sync/atomic: funcs + typed Int64/Uint64/Bool/Pointer/Value).
// ---------------------------------------------------------------------------

func runAtomics() {
	section("atomics")

	// Function-style atomics.
	var i32 int32
	for i := 0; i < 100; i++ {
		atomic.AddInt32(&i32, 1)
	}
	chk("atomic/addint32", int(atomic.LoadInt32(&i32)), 100)

	var i64 int64 = 10
	old := atomic.SwapInt64(&i64, 99)
	chk("atomic/swap-old", int(old), 10)
	chk("atomic/swap-new", int(atomic.LoadInt64(&i64)), 99)

	swapped := atomic.CompareAndSwapInt64(&i64, 99, 7)
	chkTrue("atomic/cas-ok", swapped)
	chk("atomic/cas-value", int(atomic.LoadInt64(&i64)), 7)
	notSwapped := atomic.CompareAndSwapInt64(&i64, 99, 5) // 99 != 7
	chk("atomic/cas-fail", notSwapped, false)

	var u32 uint32
	atomic.StoreUint32(&u32, 256)
	chk("atomic/store-uint32", int(atomic.LoadUint32(&u32)), 256)

	// Typed atomics (Go 1.19+): atomic.Int64, Uint64, Bool, Value, Pointer[T].
	var ai atomic.Int64
	var wg sync.WaitGroup
	for i := 1; i <= 100; i++ {
		wg.Add(1)
		go func(v int) {
			defer wg.Done()
			ai.Add(int64(v))
		}(i)
	}
	wg.Wait()
	chk("atomic/Int64-sum", int(ai.Load()), 5050)

	var au atomic.Uint64
	au.Store(1000)
	au.Add(234)
	chk("atomic/Uint64", int(au.Load()), 1234)

	var ab atomic.Bool
	chk("atomic/Bool-default", ab.Load(), false)
	ab.Store(true)
	chkTrue("atomic/Bool-store", ab.Load())
	prev := ab.Swap(false)
	chkTrue("atomic/Bool-swap-old", prev)
	chk("atomic/Bool-swap-new", ab.Load(), false)

	var ai32 atomic.Int32
	ai32.Store(5)
	chk("atomic/Int32-cas", ai32.CompareAndSwap(5, 50), true)
	chk("atomic/Int32-cas-value", int(ai32.Load()), 50)

	// atomic.Value holds an arbitrary value (type-consistent).
	var av atomic.Value
	av.Store("config-v1")
	chkStr("atomic/Value", av.Load().(string), "config-v1")
	av.Store("config-v2")
	chkStr("atomic/Value-update", av.Load().(string), "config-v2")

	// atomic.Pointer[T] holds a typed pointer.
	type cfg struct{ n int }
	var ap atomic.Pointer[cfg]
	chkTrue("atomic/Pointer-nil", ap.Load() == nil)
	ap.Store(&cfg{n: 42})
	chk("atomic/Pointer-load", ap.Load().n, 42)
	newCfg := &cfg{n: 7}
	chkTrue("atomic/Pointer-cas", ap.CompareAndSwap(ap.Load(), newCfg))
	chk("atomic/Pointer-cas-value", ap.Load().n, 7)
}
