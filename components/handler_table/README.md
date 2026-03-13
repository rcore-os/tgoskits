# handler_table

[![Crates.io](https://img.shields.io/crates/v/handler_table)](https://crates.io/crates/handler_table)
[![Docs.rs](https://docs.rs/handler_table/badge.svg)](https://docs.rs/handler_table)
[![CI](https://github.com/arceos-org/handler_table/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/arceos-org/handler_table/actions/workflows/ci.yml)

A lock-free table of event handlers.

## Examples

```rust
use handler_table::HandlerTable;

static TABLE: HandlerTable<8> = HandlerTable::new();

TABLE.register_handler(0, || {
   println!("Hello, event 0!");
});
TABLE.register_handler(1, || {
   println!("Hello, event 1!");
});

assert!(TABLE.handle(0)); // print "Hello, event 0!"
assert!(!TABLE.handle(2)); // unregistered

assert!(TABLE.unregister_handler(2).is_none());
let func = TABLE.unregister_handler(1).unwrap(); // retrieve the handler
func(); // print "Hello, event 1!"

assert!(!TABLE.handle(1)); // unregistered
```
