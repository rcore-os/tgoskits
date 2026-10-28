# LuaRocks 受控源码安装测试

本应用在 StarryOS riscv64 客户机内安装 Lua 5.4 与 LuaRocks，由 LuaRocks 从本地
rockspec 通过 HTTP 获取 `inspect` 源码并安装到默认 `/usr/local`，最后检查安装
文件、版本注册以及 `require("inspect")` 的渲染结果。配置见
[qemu-riscv64.toml](qemu-riscv64.toml)，客户机入口见
[sh/lua-luarocks-tests.sh](sh/lua-luarocks-tests.sh)。

## 1. 运行与判定

测试保留下载失败即退出的行为。原用例中客户机可以取得 rockspec，但无法从 GitHub
取得源码归档；本应用把源码归档改为宿主提供的固定 HTTP 资产。

### 1.1 运行入口

在仓库根目录执行以下命令。`prebuild.sh` 会先准备并校验资产，随后测试框架启动宿主
HTTP 服务和 QEMU。

```bash
cargo xtask starry app qemu -t qemu/lua-luarocks --arch riscv64
```

### 1.2 判定标记

成功时客户机输出 `LUA_LUAROCKS_TEST_PASSED`，失败时输出
`LUA_LUAROCKS_TEST_FAILED`。`qemu-riscv64.toml` 继续用这两个标记和内核 panic
判定结果，并保留原 `timeout = 600`。

## 2. 资产与范围

预构建与客户机分工明确：预构建只负责固定资产和宿主 HTTP 分发，客户机执行真实的
LuaRocks 解析、下载、构建与注册流程。

### 2.1 固定资产

`prebuild.sh` 固定 `inspect-3.1.3-0.rockspec` 与 `inspect.lua-3.1.3.tar.gz` 的
下载地址和 SHA-256，缓存每次运行都重新校验，哈希不匹配直接失败。tarball 留在
`target/lua-luarocks-assets`，由 `10.0.2.2:18391` 提供；rockspec 只把
`source.url` 改写为该地址并注入 `/usr/share/lua-luarocks`。由于 prebuild 会选择
plain 流水线，两个 `sh/` 脚本也由预构建安装到 `/usr/bin`。

### 2.2 验证范围

客户机通过 QEMU SLIRP 以真实 TCP 下载源码并执行 `luarocks-5.4 install`，安装后
检查 `/usr/local/share/lua/5.4/inspect.lua`、LuaRocks 记录的 `3.1.3-0` 版本以及
模块渲染输出。该路径不经过 GitHub、DNS 或 HTTPS 证书校验，因此不能用于判断外网
DNS、TLS 或代理策略是否正常。
