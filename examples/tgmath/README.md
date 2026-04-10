# tgmath

A tiny math utility crate for TGOSKits.

## Overview

This crate provides basic math utility functions:

- `add(a, b)` — Add two numbers
- `sub(a, b)` — Subtract `b` from `a`
- `clamp(val, lo, hi)` — Clamp a value within a range
- `gcd(a, b)` — Greatest common divisor (Euclidean algorithm)

The crate is `no_std` compatible.

## Usage

```rust
use tgmath::{add, sub, clamp, gcd};

assert_eq!(add(2, 3), 5);
assert_eq!(sub(5, 3), 2);
assert_eq!(clamp(15, 0, 10), 10);
assert_eq!(gcd(12, 8), 4);
```

## Running Tests

```bash
cargo test -p tgmath
cargo clippy -p tgmath -- -D warnings
```
