# task.py 使用说明

## 概述

`task.py` 是 Axvisor 项目的主要命令行工具，提供了项目的构建、运行和设置功能。它是一个统一的入口点，简化了开发和部署流程。

## 基本用法

```bash
# 激活虚拟环境
source ./activate.sh

./task.py <command> [options]
```

## 可用命令

### 1. build - 构建项目

./scripts/task.py <command> [options]
构建 Axvisor 项目。

```bash
./task.py build [options]
```

**功能**：

- 自动设置 ArceOS 依赖（如果尚未设置）
./scripts/task.py build [options]
- 执行 make 构建

**示例**：

```bash
# 使用 .hvconfig.toml 中的配置构建
./scripts/task.py build

# 指定平台构建
./scripts/task.py build --plat aarch64-generic

# 添加特性
./scripts/task.py build --features "feature1,feature2"

# 添加 ArceOS 特性
./scripts/task.py build --arceos-features "page-alloc-64g,smp"

# 添加 ArceOS Makefile 参数
./scripts/task.py build --arceos-args "NET=y,BLK=y,MEM=8g,LOG=debug"

# 指定 VM 配置文件
./scripts/task.py build --vmconfigs "config1.toml,config2.toml"
```

### 3. run - 运行项目

构建并运行 Axvisor 项目。

```bash
./scripts/task.py run [options]
```

**功能**：

- 首先执行构建步骤
- 如果构建成功，则运行项目
./scripts/task.py run

**示例**：
./scripts/task.py run --plat aarch64-generic --arceos-args "MEM=4g,BUS=mmio,BLK=y,LOG=debug" --features "fs"

```bash
# 使用 .hvconfig.toml 中的配置运行
./task.py run

# 使用特定配置运行，执行后会根据参数生成 .hvconfig.toml
./task.py run --plat aarch64-generic --arceos-args "MEM=4g,BUS=mmio,BLK=y,LOG=debug" --features "fs"
```

./scripts/task.py run --arceos-args "QEMU_ARGS=\"-smp 4 -m 2G -netdev user,id=net0\""

### 通用参数

以下参数适用于 `build` 和 `run` 命令：

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `--plat` | string | aarch64-generic | 指定目标平台 |
| `--arch` | string | 自动检测 | 指定目标架构 |
| `--package` | string | 自动检测 | 指定平台包名 |
cp .hvconfig.dev.toml .hvconfig.toml && ./scripts/task.py build
cp .hvconfig.prod.toml .hvconfig.toml && ./scripts/task.py run
| `--features` | string | 无 | Hypervisor 特性（逗号分隔） |
| `--arceos-features` | string | 无 | ArceOS 特性（逗号分隔） |
| `--arceos-args` | string | 无 | ArceOS 参数（逗号分隔） |
| `--vmconfigs` | string | 无 | VM 配置文件路径（逗号分隔） |

### 参数详解

#### --plat (平台)

指定目标平台，系统会自动从 `platform/{plat}/axconfig.toml` 读取对应的架构和包配置。

```bash
--plat aarch64-generic
--plat x86-qemu-q35
```

**架构特定的 QEMU 参数**：

- `aarch64`: `-machine virtualization=on,gic-version=3`
- `x86_64`: `-enable-kvm -cpu host`
- `riscv64`: `-machine virt -cpu rv64`

#### --features (Hypervisor 特性)

指定 Hypervisor 的特性，多个特性用逗号分隔。

```bash
--features "fs"
```

#### --arceos-features (ArceOS 特性)

指定 ArceOS 的特性，多个特性用逗号分隔。

```bash
--arceos-features "page-alloc-64g"
--arceos-features "smp,net,blk"
```

#### --arceos-args (ArceOS 参数)

指定传递给 ArceOS 的参数，支持键值对和标志，多个参数用逗号分隔。

```bash
--arceos-args "NET=y,BLK=y,MEM=8g"
--arceos-args "SMP=4,LOG=debug"
```

#### --vmconfigs (VM 配置文件)

指定 VM 配置文件的路径，多个文件用逗号分隔。

```bash
--vmconfigs "vm1.toml"
--vmconfigs "vm1.toml,vm2.toml,vm3.toml"
```

## 配置文件

`task.py` 支持通过 `.hvconfig.toml` 配置文件设置默认参数，命令行参数会覆盖配置文件中的设置。

### 配置文件示例

创建 `.hvconfig.toml` 文件：

```toml
plat = "aarch64-generic"
arceos_args = [ "BUS=mmio", "BLK=y", "MEM=8g", "LOG=debug", "QEMU_ARGS=\"-machine gic-version=3\""]
vmconfigs = [ "tmp/arceos-aarch64.toml",]
```

### 配置优先级

1. **命令行参数** (最高优先级)
2. **配置文件** (.hvconfig.toml)

## 高级用法

### 1. 自定义 QEMU 参数

```bash
# 添加自定义 QEMU 参数，会与架构特定参数合并
./task.py run --arceos-args "QEMU_ARGS=\"-smp 4 -m 2G -netdev user,id=net0\""

# 对于 aarch64，最终的 QEMU_ARGS 会是：
# "-smp 4 -m 2G -netdev user,id=net0 -machine virtualization=on"
```

### 4. 批量配置

创建多个配置文件用于不同的开发场景：

```bash
# 开发配置
cp .hvconfig.toml .hvconfig.dev.toml
# 编辑 .hvconfig.dev.toml 添加调试参数

# 生产配置  
cp .hvconfig.toml .hvconfig.prod.toml
# 编辑 .hvconfig.prod.toml 优化性能参数

# 使用不同配置
cp .hvconfig.dev.toml .hvconfig.toml && ./task.py build
cp .hvconfig.prod.toml .hvconfig.toml && ./task.py run
```

## 故障排除

### 常见问题

1. **构建失败**

   ```bash
   # 清理并重新设置
   rm -rf .arceos
   ./task.py clean
   ./task.py build
   ```

2. **平台配置找不到**

   ```text
   警告：平台配置文件 platform/xxx/axconfig.toml 不存在
   ```

   - 检查平台名称是否正确
   - 确保对应的平台目录存在

3. **TOML 库缺失**

   ```text
   警告：需要安装 toml 库来读取配置文件
   ```

   ```bash
   pip install toml
   ```

4. **权限问题**

   ```bash
   # 确保 task.py 有执行权限
   chmod +x task.py
   ```

### 调试技巧

1. **查看生成的构建命令**

   ```python
   from scripts.config import AxvisorConfig
   config = AxvisorConfig()
   print(config.format_make_command("build"))
   ```

2. **检查配置合并结果**

   ```python
   from scripts.config import AxvisorConfig
   config = AxvisorConfig(plat="aarch64-generic")
   print(f"Platform: {config.plat}")
   print(f"Architecture: {config.arch}")
   print(f"Package: {config.package}")
   print(f"Make vars: {config.get_make_variables()}")
   ```

## 开发扩展

如果需要添加新的命令或功能：

1. **添加新命令**：
   - 在 `scripts/` 目录下创建新的 Python 模块
   - 在 `task.py` 中添加对应的子解析器
   - 实现 `main(args)` 函数

2. **添加新参数**：
   - 修改 `scripts/config.py` 中的 `add_common_arguments` 函数
   - 更新 `AxvisorConfig` 类的字段
   - 更新相关的处理逻辑
