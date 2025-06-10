# lazyinit

[![Crates.io](https://img.shields.io/crates/v/lazyinit)](https://crates.io/crates/lazyinit)
[![Docs.rs](https://docs.rs/lazyinit/badge.svg)](https://docs.rs/lazyinit)
[![CI](https://github.com/arceos-org/lazyinit/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/arceos-org/lazyinit/actions/workflows/ci.yml)

Initialize a static value lazily.

The crate provides a type for initializing static values lazily in a thread-safe manner.  
Unlike compile-time initialization or macro-based solutions like [`lazy_static`][1], this type allows runtime initialization with arbitrary logic while guaranteeing that initialization occurs exactly once across all threads.

The core abstraction is a struct that wraps a value and manages its initialization state through atomic operations. The value remains uninitialized until the first call to `init_once` or `call_once`, at which point it becomes permanently initialized and accessible.

[1]: https://docs.rs/lazy_static

## Features

- Thread-Safe Initialization: Guarantees exactly one initialization across multiple threads
- Flexible Initialization: Supports both direct value initialization and closure-based initialization
- Safe Access Patterns: Provides both safe and unsafe access methods
- State Inspection: Allows checking initialization status without accessing the value
- Direct Access: Implements `Deref` and `DerefMut` for transparent access after initialization
- No-std Compatibility: Works in embedded and kernel environments without the standard library. No external dependencies.

## Examples

```rust
use lazyinit::LazyInit;

static VALUE: LazyInit<u32> = LazyInit::new();
assert!(!VALUE.is_inited());
// println!("{}", *VALUE); // panic: use uninitialized value
assert_eq!(VALUE.get(), None);

VALUE.init_once(233);
// VALUE.init_once(666); // panic: already initialized
assert!(VALUE.is_inited());
assert_eq!(*VALUE, 233);
assert_eq!(VALUE.get(), Some(&233));
```

Only one of the multiple initializations can succeed:

```rust
use lazyinit::LazyInit;
use std::time::Duration;

const N: usize = 16;
static VALUE: LazyInit<usize> = LazyInit::new();

let threads = (0..N)
    .map(|i| {
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            VALUE.call_once(|| i)
        })
    })
    .collect::<Vec<_>>();

let mut ok = 0;
for (i, thread) in threads.into_iter().enumerate() {
    if thread.join().unwrap().is_some() {
        ok += 1;
        assert_eq!(*VALUE, i);
    }
}

assert_eq!(ok, 1);
```
