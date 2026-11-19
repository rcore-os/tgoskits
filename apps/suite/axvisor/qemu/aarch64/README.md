# AxVisor 手动 QEMU 场景

这些场景由人进入控制台或访问网页检查，不登记普通 CI、夜间或 benchmark。原 `test-suit/axvisor/manual` 的七份配置已迁到这里，原启动参数保持；自动步骤场景 `console/` 的运行规则仍见其独立 README。

## 1. 场景配置

目录按使用目的组织。构建配置决定 features 和候选客户机，QEMU 配置决定机器、磁盘、端口与调试输出；文件存在或启动成功不代表人工检查已通过。

### 1.1 API与网页

`http-control-plane/qemu-aarch64-hostfwd.toml`把本机8080转发到AxVisor API。它只提供手动运行参数，构建仍显式复用自动套件的配方与客户机资产；该依赖不是通过目录扫描隐式取得。`web-ui-demo/`则包含自己的`demo-fs-build.toml`，启用完整Web UI与NVMe，未预置客户机，由操作者向动态客户机目录添加配置。

| 场景 | 构建输入 | 运行输入 | 人工检查 |
| --- | --- | --- | --- |
| http-control-plane | test-suit/axvisor/qemu/aarch64/http-control-plane的构建配置 | 本目录hostfwd配置 | API响应、启动与删除客户机后的状态 |
| web-ui-demo | demo-fs-build.toml | 本目录hostfwd配置 | 网页加载、动态池、客户机与终端交互 |

API配方中的BusyBox initramfs等资产仍须按自动套件的构建条件准备；这里只移动手动入口，没有复制或重写那条准备链路。当前默认管理地址是`http://localhost:8080/`。

### 1.2 三客户机控制台

`browser-console-3vm/`保留普通与`intlog`两套构建/运行配置，分别使用匹配的`build-*.toml`和`qemu-*.toml`。二者复用`os/axvisor/configs/vms/qemu/aarch64/arceos-fs-guest{1,2,3}.toml`，启动命令行显式选择三个客户机。客户机可以打印后退出，人工检查关注独立控制台与输出归属，不是持续三客户机压力。

普通变体有NVMe、4GiB；intlog变体为2GiB、无NVMe，并开启QEMU中断/guest_errors日志。迁移保留了这些差异，未将intlog改成普通配置加日志。其磁盘与文件系统资产条件需要使用者在运行时核对，本次未实跑，不声明两个变体都可用。

## 2. 启动与维护

从TGOSKits根目录运行以下直接QEMU命令，保留交互会话；这些命令不运行自动HTTP探针。原`--test-group manual`发现入口已移除，不能再从test-suit运行。

### 2.1 直接启动

API入口显式选择现有构建配方；网页和三客户机场景使用本目录自己的构建文件。构建所需镜像、交叉工具链和网页构建环境遵循仓库原要求。

```sh
# 手动访问API，构建与客机资源复用现有配方。
cargo xtask axvisor qemu --arch aarch64 \
  -c test-suit/axvisor/qemu/aarch64/http-control-plane/build-aarch64-unknown-none-softfloat.toml \
  --qemu-config apps/suite/axvisor/qemu/aarch64/http-control-plane/qemu-aarch64-hostfwd.toml

# 完整网页演示，不预置客户机。
cargo xtask axvisor qemu --arch aarch64 \
  -c apps/suite/axvisor/qemu/aarch64/web-ui-demo/demo-fs-build.toml \
  --qemu-config apps/suite/axvisor/qemu/aarch64/web-ui-demo/qemu-aarch64-hostfwd.toml

# 三个客户机控制台；intlog变体同时替换下面两个配置的文件名。
cargo xtask axvisor qemu --arch aarch64 \
  -c apps/suite/axvisor/qemu/aarch64/browser-console-3vm/build-aarch64-unknown-none-softfloat.toml \
  --qemu-config apps/suite/axvisor/qemu/aarch64/browser-console-3vm/qemu-aarch64.toml
```

网页演示启用`web-ui`，需要Node 24及npm 11以上。检查结束后在QEMU控制台使用`Ctrl-a x`退出；关闭网页不会停止QEMU。

### 2.2 修改约束

固定端口8080、三客户机调试端口1234及`/tmp/qemu-mon.sock`目前仅适合单实例；intlog还写`/tmp/qemu-int.log`。同时运行多个实例前，须为各实例调整这些地址。迁移没有修改共享环境或全局工具配置。

新增人工场景按用途放独立目录并写明构建、资产、检查和退出方式；已有场景的兼容步骤优先共用启动。只有实际共享的配置才放common。若接入自动测试，应补充能传播失败的检查并明确登记，不能把无成功条件的人工配置当作自动通过。
