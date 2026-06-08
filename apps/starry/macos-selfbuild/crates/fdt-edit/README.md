# fdt-edit

[![Crates.io](https://img.shields.io/crates/v/fdt-edit.svg)](https://crates.io/crates/fdt-edit)
[![Documentation](https://docs.rs/fdt-edit/badge.svg)](https://docs.rs/fdt-edit)

`fdt-edit` is a pure-Rust, `#![no_std]` library for creating, loading, editing, querying, and re-encoding Flattened Device Tree (FDT) blobs.

The crate is intended for firmware, kernels, bootloaders, and embedded tooling that need a mutable in-memory device tree representation instead of a read-only parser.

## What It Does

- Parse existing DTB data into an editable arena-backed tree
- Build new device trees programmatically from scratch
- Add, update, and remove nodes and properties
- Query nodes by path, phandle, or compatible string
- Re-encode the edited tree back into DTB bytes
- Work in `no_std` environments with `alloc`

## Why `fdt-edit`

This repository originally focused on parsing. The current high-level crate is `fdt-edit`, which sits on top of `fdt-raw` and provides a mutable API for real tree manipulation.

Compared with a read-only parser, `fdt-edit` is designed for workflows such as:

- patching a board DTB before boot
- constructing a synthetic tree in tests
- rewriting properties like `reg`, `status`, `compatible`, or `interrupt-parent`
- preserving memory reservation entries while round-tripping DTB data

## Installation

Add the crate to your `Cargo.toml`:

```toml
[dependencies]
fdt-edit = "0.2.0"
```

## Quick Start

### Load, Modify, Encode

```rust,no_run
use fdt_edit::{Fdt, Node, Property};

# fn main() -> Result<(), fdt_edit::FdtError> {
let dtb: &[u8] = &[]; // replace with real DTB bytes
let mut fdt = Fdt::from_bytes(dtb)?;

let root_id = fdt.root_id();
fdt.node_mut(root_id)
  .unwrap()
  .set_property(Property::new("model", b"example-board\0".to_vec()));

let soc_id = if let Some(node) = fdt.get_by_path("/soc") {
  node.id()
} else {
  fdt.add_node(root_id, Node::new("soc"))
};

let mut uart = Node::new("uart@1000");
uart.set_property(Property::new("compatible", b"ns16550a\0".to_vec()));
uart.set_property(Property::new(
  "reg",
  vec![0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x01, 0x00],
));
fdt.add_node(soc_id, uart);

let encoded = fdt.encode();
assert!(!encoded.is_empty());
# Ok(())
# }
```

### Build A New Tree

```rust
use fdt_edit::{Fdt, Node, Property};

let mut fdt = Fdt::new();
let root_id = fdt.root_id();

fdt.node_mut(root_id).unwrap().set_property(Property::new(
  "#address-cells",
  2u32.to_be_bytes().to_vec(),
));
fdt.node_mut(root_id).unwrap().set_property(Property::new(
  "#size-cells",
  1u32.to_be_bytes().to_vec(),
));

let mut memory = Node::new("memory@80000000");
memory.set_property(Property::new("device_type", b"memory\0".to_vec()));
memory.set_property(Property::new(
  "reg",
  vec![
    0x80, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00,
    0x40, 0x00, 0x00, 0x00,
  ],
));
fdt.add_node(root_id, memory);

let dtb = fdt.encode();
assert!(dtb.len() >= 40);
```

### Query Typed Nodes

```rust,no_run
use fdt_edit::{Fdt, NodeType};

# fn main() -> Result<(), fdt_edit::FdtError> {
# let dtb: &[u8] = &[]; // replace with real DTB bytes
let fdt = Fdt::from_bytes(dtb)?;

for node in fdt.find_compatible(&["pci-host-ecam-generic"]) {
  if let NodeType::Pci(pci) = node {
    let _bus_range = pci.bus_range();
    let _interrupt_cells = pci.interrupt_cells();
  }
}
# Ok(())
# }
```

## Core API

### `Fdt`

- `Fdt::new()`: create an empty editable tree
- `Fdt::from_bytes()`: parse DTB bytes into an editable tree
- `Fdt::from_ptr()`: parse from a raw pointer
- `root_id()`: get the root node ID
- `node()` / `node_mut()`: access raw mutable nodes by ID
- `add_node()`: insert a child node
- `remove_node()` / `remove_by_path()`: delete nodes and subtrees
- `get_by_path()`: fetch a classified node view by absolute path or alias
- `get_by_phandle()`: fetch a node by phandle
- `find_compatible()`: search by compatible string
- `all_nodes()`: depth-first iteration over the whole tree
- `encode()`: serialize the tree back into DTB bytes

### `Node`

- `Node::new(name)`: create a node
- `set_property()`: add or replace a property
- `remove_property()`: delete a property
- `get_property()`: inspect a property
- `children()`: list child node IDs
- helpers like `address_cells()`, `size_cells()`, `phandle()`, `compatible()`, `status()`

### `Property`

- `Property::new(name, data)`: create a raw property
- `get_u32()` / `get_u64()`: decode integer values
- `set_u32_ls()` / `set_u64()`: encode integer values
- `as_str()` / `as_str_iter()`: decode string and string-list properties
- `set_string()` / `set_string_ls()`: update string data

## Typed Node Views

`get_by_path()`, `get_by_phandle()`, and `all_nodes()` return classified node views, so code can branch on device-tree semantics instead of only raw node names.

Available typed views include:

- `NodeType::Generic`
- `NodeType::Memory`
- `NodeType::InterruptController`
- `NodeType::Clock`
- `NodeType::Pci`

These views expose helpers such as inherited `interrupt-parent` lookup, translated `reg` handling, clock metadata, memory region inspection, and PCI-specific range or interrupt-map parsing.

## Encoding And Round-Tripping

`fdt-edit` preserves the parts of the tree that matter for boot-time DTB generation:

- header metadata such as `boot_cpuid_phys`
- memory reservation entries
- node hierarchy and property ordering
- string table regeneration during encoding

The crate is built for parse-edit-encode workflows and includes tests that round-trip real DTBs from several platforms.

## Repository Layout

This repository is a small workspace:

- `fdt-edit`: the high-level editable FDT library described in this README
- `fdt-raw`: lower-level parsing and data primitives used by `fdt-edit`
- `dtb-file`: DTB fixtures used by tests and examples

## Testing

```bash
cargo test -p fdt-edit
```

The test suite covers:

- parsing real DTB fixtures
- tree traversal and path lookup
- typed node classification
- inherited interrupt-parent resolution
- DTB encoding and round-trip correctness
- memory reservation serialization

## License

`fdt-edit` is licensed under `MIT OR Apache-2.0`.

## Repository

https://github.com/drivercraft/fdt-parser