# ax-config

Platform-specific constants and parameters for [ArceOS](https://github.com/arceos-org/arceos).

Uses [`ax-config-macros`](https://docs.rs/ax-config-macros) to generate compile-time configuration from a TOML file. Set the `AX_CONFIG_PATH` environment variable to point to a custom config; otherwise a built-in `dummy.toml` is used as fallback.

## Usage

```toml
[dependencies]
ax-config = "0.2"
```

## License

GPL-3.0-or-later OR Apache-2.0 OR MulanPSL-2.0
