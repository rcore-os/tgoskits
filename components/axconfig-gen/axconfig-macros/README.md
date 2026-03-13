# axconfig-macros

Procedural macros for converting TOML format configurations to equivalent Rust constant definitions.

## Example

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

Value types are necessary for generating Rust constant definitions. Types can be specified by the comment following the config item. Currently supported types are `bool`, `int`, `uint`, `str`, `(type1, type2, ...)` for tuples, and `[type]` for arrays. If no type is specified, it will try to infer the type from the value.

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

```rust,ignore
axconfig_macros::include_configs!("path/to/config.toml");
// or specify the config file path via an environment variable
axconfig_macros::include_configs!(path_env = "AX_CONFIG_PATH");
// or with a fallback path if the environment variable is not set
axconfig_macros::include_configs!(path_env = "AX_CONFIG_PATH", fallback = "path/to/defconfig.toml");
```
