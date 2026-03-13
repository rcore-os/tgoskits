# cap_access

[![Crates.io](https://img.shields.io/crates/v/cap_access)](https://crates.io/crates/cap_access)
[![Docs.rs](https://docs.rs/cap_access/badge.svg)](https://docs.rs/cap_access)
[![CI](https://github.com/arceos-org/cap_access/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/arceos-org/cap_access/actions/workflows/ci.yml)

Provide basic [capability-based][1] access control to objects.

The wrapper type [`WithCap`] associates a **capability** to an object, that
is a set of access rights. When accessing the object, we must explicitly
specify the access capability, and it must not violate the capability
associated with the object at initialization.

## Examples

```rust
use cap_access::{Cap, WithCap};

let data = WithCap::new(42, Cap::READ | Cap::WRITE);

// Access with the correct capability.
assert_eq!(data.access(Cap::READ).unwrap(), &42);
assert_eq!(data.access(Cap::WRITE).unwrap(), &42);
assert_eq!(data.access(Cap::READ | Cap::WRITE).unwrap(), &42);

// Access with the incorrect capability.
assert!(data.access(Cap::EXECUTE).is_none());
assert!(data.access(Cap::READ | Cap::EXECUTE).is_none());
```

[1]: https://en.wikipedia.org/wiki/Capability-based_security
[`WithCap`]: https://docs.rs/capability/latest/capability/struct.WithCap.html

