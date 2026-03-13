# axerrno

[![Crates.io](https://img.shields.io/crates/v/axerrno)](https://crates.io/crates/axerrno)
[![Docs.rs](https://docs.rs/axerrno/badge.svg)](https://docs.rs/axerrno)
[![CI](https://github.com/arceos-org/axerrno/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/arceos-org/axerrno/actions/workflows/ci.yml)

Generic error code representation.

It provides two error types and the corresponding result types:

- [`AxError`] and [`AxResult`]: A generic error type similar to
[`std::io::ErrorKind`].
- [`LinuxError`] and [`LinuxResult`]: Linux specific error codes defined in
`errno.h`. It can be converted from [`AxError`].

[`AxError`]: https://docs.rs/axerrno/latest/axerrno/enum.AxError.html
[`AxResult`]: https://docs.rs/axerrno/latest/axerrno/type.AxResult.html
[`LinuxError`]: https://docs.rs/axerrno/latest/axerrno/enum.LinuxError.html
[`LinuxResult`]: https://docs.rs/axerrno/latest/axerrno/type.LinuxResult.html
[`std::io::ErrorKind`]: https://doc.rust-lang.org/std/io/enum.ErrorKind.html
