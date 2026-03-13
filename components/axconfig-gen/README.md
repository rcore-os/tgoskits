# axconfig-gen

[![CI](https://github.com/arceos-org/axconfig-gen/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/arceos-org/axconfig-gen/actions/workflows/ci.yml)

* [axconfig-gen](axconfig-gen): A TOML-based configuration generation tool for [ArceOS](https://github.com/arceos-org/arceos). [![Crates.io](https://img.shields.io/crates/v/axconfig-gen)](https://crates.io/crates/axconfig-gen)[![Docs.rs](https://docs.rs/axconfig-gen/badge.svg)](https://docs.rs/axconfig-gen)
* [axconfig-macros](axconfig-macros): Procedural macros for converting TOML format configurations to Rust constant definitions. [![Crates.io](https://img.shields.io/crates/v/axconfig-macros)](https://crates.io/crates/axconfig-macros)[![Docs.rs](https://docs.rs/axconfig-macros/badge.svg)](https://docs.rs/axconfig-macros)

### Executable Usage

```text
Usage: axconfig-gen [OPTIONS] <SPEC>...

Arguments:
  <SPEC>...  Paths to the config specification files

Options:
  -c, --oldconfig <OLDCONFIG>  Path to the old config file
  -o, --output <OUTPUT>        Path to the output config file
  -f, --fmt <FMT>              The output format [default: toml] [possible values: toml, rust]
  -r, --read <RD_CONFIG>       Getting a config item with format `table.key`
  -w, --write <WR_CONFIG>      Setting a config item with format `table.key=value`
  -v, --verbose                Verbose mode
  -h, --help                   Print help
  -V, --version                Print version
```

For example, to generate a config file `.axconfig.toml` from the config specifications distributed in `a.toml` and `b.toml`, you can run:

```console
$ axconfig-gen a.toml b.toml -o .axconfig.toml -f toml
```

See [defconfig.toml](example-configs/defconfig.toml) for an example of a config specification file.

Value types are necessary for generating Rust constant definitions. Types can be specified by the comment following the config item. Currently supported types are `bool`, `int`, `uint`, `str`, `(type1, type2, ...)` for tuples, and `[type]` for arrays. If no type is specified, it will try to infer the type from the value.

### Library Usage

```rust
use axconfig_gen::{Config, OutputFormat};

let config_toml = r#"
are-you-ok = true
one-two-three = 123

[hello]
"one-two-three" = "456"     # int
array = [1, 2, 3]           # [uint]
tuple = [1, "abc", 3]
"#;

let config = Config::from_toml(config_toml).unwrap();
let rust_code = config.dump(OutputFormat::Rust).unwrap();

assert_eq!(rust_code,
r#"pub const ARE_YOU_OK: bool = true;
pub const ONE_TWO_THREE: usize = 123;

pub mod hello {
    pub const ARRAY: &[usize] = &[1, 2, 3];
    pub const ONE_TWO_THREE: isize = 456;
    pub const TUPLE: (usize, &str, usize) = (1, "abc", 3);
}
"#);
```

### Macro Usage

```rust
axconfig_macros::parse_configs!(r#"
are-you-ok = true
one-two-three = 123

[hello]
"one-two-three" = "456"     # int
array = [1, 2, 3]           # [uint]
tuple = [1, "abc", 3]
"#);

assert_eq!(ARE_YOU_OK, true);
assert_eq!(ONE_TWO_THREE, 123usize);
assert_eq!(hello::ONE_TWO_THREE, 456isize);
assert_eq!(hello::ARRAY, [1, 2, 3]);
assert_eq!(hello::TUPLE, (1, "abc", 3));
```

The above example will generate the following constants:

```rust
pub const ARE_YOU_OK: bool = true;
pub const ONE_TWO_THREE: usize = 123;

pub mod hello {
    pub const ARRAY: &[usize] = &[1, 2, 3];
    pub const ONE_TWO_THREE: isize = 456;
    pub const TUPLE: (usize, &str, usize) = (1, "abc", 3);
}
```

You can also include the configuration file directly:

```rust
axconfig_macros::include_configs!("path/to/config.toml");
// or specify the config file path via an environment variable
axconfig_macros::include_configs!(path_env = "AX_CONFIG_PATH");
// or with a fallback path if the environment variable is not set
axconfig_macros::include_configs!(path_env = "AX_CONFIG_PATH", fallback = "path/to/defconfig.toml");
```
