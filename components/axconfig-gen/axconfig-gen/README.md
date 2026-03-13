# axconfig-gen

A TOML-based configuration generation tool for [ArceOS](https://github.com/arceos-org/arceos).

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

See [defconfig.toml](https://github.com/arceos-org/axconfig-gen/blob/main/example-configs/defconfig.toml) for an example of a config specification file.

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

### Related libraries

There is also a procedural macro library [`axconfig-macros`](https://docs.rs/axconfig-macros) that can be
used to include TOML files in your project and convert them to Rust code at
compile time.
