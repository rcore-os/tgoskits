# int_ratio

[![Crates.io](https://img.shields.io/crates/v/int_ratio)](https://crates.io/crates/int_ratio)
[![Docs.rs](https://docs.rs/int_ratio/badge.svg)](https://docs.rs/int_ratio)
[![CI](https://github.com/arceos-org/int_ratio/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/arceos-org/int_ratio/actions/workflows/ci.yml)

The type of ratios and related operations.

A **ratio** is the result of dividing two **integers**, i.e., the numerator and
denominator.

## Examples

```rust
use int_ratio::Ratio;

let ratio = Ratio::new(1, 3); // 1 / 3
assert_eq!(ratio.mul_trunc(20), 6); // trunc(20 * 1 / 3) = trunc(6.66..) = 6
assert_eq!(ratio.mul_round(20), 7); // round(20 * 1 / 3) = round(6.66..) = 7
println!("{:?}", ratio); // Ratio(1/3 ~= 1431655765/4294967296)
```
