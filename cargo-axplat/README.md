# cargo-axplat

[![Crates.io](https://img.shields.io/crates/v/cargo-axplat)](https://crates.io/crates/cargo-axplat)
[![CI](https://github.com/arceos-org/axplat_crates/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/arceos-org/axplat_crates/actions/workflows/ci.yml)

A cargo subcommand to manage hardware platform packages using [axplat](https://github.com/arceos-org/axplat_crates/tree/main/axplat).

## Install

```console
$ cargo install --locked cargo-axplat
```

## Usage

```text
Usage: cargo axplat [COMMAND]

Commands:
  new   Create a new platform package
  add   Add platform package dependencies to a Cargo.toml manifest file
  info  Display information about a platform package
  help  Print this message or the help of the given subcommand(s)

Options:
  -V, --version  Print version
  -h, --help     Print help
```

## Examples

### 1. Create a new platform package

It will create a new platform package named `axplat-aarch64-my-plat` in the current directory:

```console
$ cargo axplat new axplat-aarch64-my-plat --arch aarch64
    Creating library `axplat-aarch64-my-plat` package
note: see more `Cargo.toml` keys and their definitions at https://doc.rust-lang.org/cargo/reference/manifest.html
```

### 2. Add the platform package as dependency

Run in your project directory:

```console
$ cargo axplat add axplat-aarch64-my-plat --path ./axplat-aarch64-my-plat
      Adding axplat-aarch64-my-plat (local) to dependencies
    Updating crates.io index
      Adding axplat-aarch64-my-plat v0.1.0 (/home/user/my-project/axplat-aarch64-my-plat)
```

It will add `axplat-aarch64-my-plat` as a dependency in your project's `Cargo.toml` file:

```toml
[dependencies]
axplat-aarch64-my-plat = { path = "./axplat-aarch64-my-plat" }
```

### 3. Display information about the platform package

```console
$ cargo axplat info axplat-aarch64-my-plat
platform: axplat-aarch64-my-plat
arch: aarch64
version: 0.1.0
source: path+file:///home/user/my-project/axplat-aarch64-my-plat#0.1.0
manifest_path: /home/user/my-project/axplat-aarch64-my-plat/Cargo.toml
config_path: /home/user/my-project/axplat-aarch64-my-plat/axconfig.toml
```
