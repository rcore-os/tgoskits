# axio

[![Crates.io](https://img.shields.io/crates/v/axio)](https://crates.io/crates/axio)
[![Docs.rs](https://docs.rs/axio/badge.svg)](https://docs.rs/axio)
[![CI](https://github.com/arceos-org/axio/actions/workflows/check.yml/badge.svg?branch=main)](https://github.com/arceos-org/axio/actions/workflows/check.yml)

[`std::io`][1] for `no_std` environment.

[1]: https://doc.rust-lang.org/std/io/index.html

### Features

- **alloc**:
  - Enables extra methods on `Read`: `read_to_end`, `read_to_string`.
  - Enables extra methods on `BufRead`: `read_until`, `read_line`, `split`, `lines`.
  - Enables implementations of axio traits for `alloc` types like `Vec<u8>`, `Box<T>`, etc.

### Differences to `std::io`

- Error types from `axerrno` instead of `std::io::Error`.
- No `IoSlice` and `*_vectored` APIs.

### Limitations

- Requires nightly Rust.

## License

Licensed under either of

- GNU General Public License v3.0 or later, (<https://www.gnu.org/licenses/gpl-3.0.html>)
- Apache License, Version 2.0, (<https://www.apache.org/licenses/LICENSE-2.0>)
- Mulan Permissive Software License, Version 2, (<https://license.coscl.org.cn/MulanPSL2>)

at your option.

---

Almost all of the code in this repository is a copy of the [Rust language codebase](https://github.com/rust-lang/rust) with minor modifications.

For attributions, see <https://thanks.rust-lang.org/>.
