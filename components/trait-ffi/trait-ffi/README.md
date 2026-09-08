# trait-ffi 静态接口

`trait-ffi` 在接口软件包中定义 trait，由最终系统提供唯一实现，消费方通过生成的普通函数调用。公共入口为 `#![no_std]` 软件包，`trait-ffi-macros` 仅在编译器宿主端运行，使用工作区的 `syn 3`。

## 1. 接口装配

`def_extern_trait` 同时生成调用模块和模块内的 `impl_trait!`。实现宏掌握原始声明，为全部方法生成精确签名的静态导出，包括实现方省略的默认方法；不使用运行时登记、虚表或弱符号。

### 1.1 定义与实现

以下示例展示单个接口的完整装配。实际项目可将 trait 放在接口包中，将实现放在最终系统包中；实现包不需要直接依赖宏包。

```rust
use trait_ffi::def_extern_trait;

#[def_extern_trait]
pub trait Clock {
    fn ticks() -> u64;
    fn doubled() -> u64 { Self::ticks() * 2 }
}

struct Platform;
clock::impl_trait! {
    impl Clock for Platform {
        fn ticks() -> u64 { 21 }
    }
}
assert_eq!(clock::doubled(), 42);
```

`Clock` 在宏输入中使用声明时的名称，无需导入 trait；`Platform` 可以是类型路径或具体的泛型实例。调用 trait 关联函数时仍遵循普通 Rust 的 trait 导入规则。

### 1.2 路径与配置

嵌套接口通过 `mod_path` 指定相对于接口包根的真实模块路径，例如 `#[def_extern_trait(mod_path = "platform")]`。调用生成宏中的 `$crate` 指向接口包，因此实现方可以重命名 Cargo 依赖。`definition::expand` 在定义包中选择 `cfg` 分支，不要求实现包拥有同名 feature。

| 选项 | 行为 |
| --- | --- |
| `abi = "Rust"` | 默认 Rust ABI；也接受 `"rust"` |
| `abi = "C"` | C ABI；也接受 `"c"`，拒绝编译器识别的非 FFI 安全类型 |
| `mod_path = "platform"` | trait 的实际定义模块，必须从接口包根可访问 |
| `namespace = "Board"` | 额外链接隔离标识 |
| `impl_macro = "bind_clock"` | 在定义模块额外公开具名实现宏；通常使用 `clock::impl_trait!` 即可 |

未知、重复或格式错误的选项由 `args::Args` 返回带源码位置的错误。类型名、参数和返回值由接口定义拥有；实现方不必为省略的默认方法导入接口包内部使用的类型。

## 2. 链接契约

`args::link_prefix` 将协议版本、定义包名、兼容版本、模块路径、namespace 和 trait 名组合为链接身份，再附加方法名。身份按字节编码，避免连字符、下划线和模块分隔符被规范化成同一名字。它不是签名散列或运行时 ABI 检查器。

### 2.1 唯一实现

同一最终链接单元内，一个接口只能绑定一次。重复绑定形成重复符号；调用未绑定的接口形成未定义符号。不同 trait 的同名方法、不同模块的同名 trait 均由链接身份隔离。提供者必须真实链接进最终程序，例如通过 `use provider_crate as _;` 保留仅负责导出的依赖。

兼容版本遵循 Cargo 语义：`1.x` 使用主版本，`0.x` 使用主次版本，`0.0.x` 使用完整版本；预发布版本保留完整版本。破坏接口或布局的变更必须升级相应兼容版本，声明方与实现方必须使用兼容的类型依赖及构建配置。

### 2.2 安全边界

安全包装只承担自动生成的外部声明与导出一致性。`unsafe fn` 的调用前置条件仍由调用方承担，生成的调用函数保留 `unsafe`；`unsafe trait` 的绑定必须显式写 `unsafe impl`。导出函数通过限定 trait 调用提供者，默认方法内的 `Self::method()` 因而仍选择同一提供者。

Rust ABI 要求兼容的 Rust 构建环境，不能用于跨编译器版本的稳定动态插件。C ABI 要求参数、返回值及外部调用满足 Rust 的布局、有效值、生命周期和所有权规则；编译器的 FFI lint 不能验证外部调用者是否遵守这些契约。C ABI 不支持 panic 穿过边界。

## 3. 支持边界

`signature::validate` 在代码生成前拒绝不受支持的语法。`signature::export_signature` 为具体参数类型建立定义侧别名，显式保留借用生命周期，使实现宏无需重新猜测类型或 ABI。

### 3.1 支持的函数

支持同步关联函数、普通和 unsafe 方法、默认方法、具体参数及返回类型、函数指针、匿名参数和普通借用。借用返回遵循单一输入生命周期的省略规则，也可显式返回 `'static`。直接 `#[cfg(...)]` 在定义包中选择导出，方法被关闭时不会解析其不可用类型。

### 3.2 明确拒绝的语法

不支持 receiver、trait 或方法泛型、where 子句、supertrait、关联类型或常量、签名中的 `Self`、`impl Trait`、类型宏、async、const 方法和可变参数。ABI 在 trait 上统一选择。当前不支持 `cfg_attr`，需使用直接的 `cfg`。这是一组全局静态能力接口，不是任意 Rust trait 的跨语言对象转换器。

## 4. 来源与迁移

代码源自 `ZR233/trait-ffi` 的 `0.2.11`，对应上游提交 `61379b7043341dced18b03245cc85a46aef85a33`。上游 manifest 和 README 声明 MIT，未附独立 LICENSE；本地保留作者及 MIT 声明，并随两个软件包提供许可证正文。

### 4.1 本地维护

本地将公共入口与编译器宿主端宏实现分为 `trait-ffi` 和 `trait-ffi-macros`，均为 `0.3.0`。公共包对宏包采用精确版本依赖，使生成协议同步。原始生成器使用 `syn 2`；当前代码使用 `syn 3` 的 `Signature::safety`、`TypeFnPtr` 和 modifier 校验接口，不依赖 `syn 2`。

继续外部依赖会保留宏重名、默认方法缺少导出和路径硬编码问题；改用 `ax-crate-interface` 又不符合本次维护 trait-ffi 的目标。本次选择本地维护并由接口定义生成所有导出，移除实现侧重复配置 `name`、ABI、版本的低层 `impl_extern_trait` 入口，以及旧 `not_def_impl` 绕行选项。

### 4.2 调用方迁移

现有 `axklib`、`crab-usb` 使用 workspace 依赖；运行时和驱动测试从 `axklib::klib::impl_trait` 导入实现宏。消费方普通函数路径不变。`0.3.0` 的符号带新生成协议前缀，不能与旧版本生成的符号混用；回滚必须同时恢复依赖、实现宏导入和全部相关构建产物。本改动没有持久状态或运行时注册状态。

## 5. 验证证据

`tests/default_methods.rs` 是缺失默认导出的确定性回归：上游实现仅适配 syn 3 后，链接因 `__trait_ffi_0_3_inherited` 未定义而失败；修复后同一断言返回 42。`tests/interfaces.rs` 覆盖同名方法、C ABI、borrow 和 unsafe 默认方法；`tests/cross_crate.rs` 使用独立 no_std 定义和提供者包，验证依赖改名、私有导入类型及定义侧 feature。`relative_type_and_array_paths_resolve_in_the_definition_module` 在修复前因 `super::Sample` 无法解析而失败；`signature::DefinitionPaths` 按生成模块的深度保留相对路径的原始指向。

`tests/diagnostics.rs` 使用 trybuild 核对错误输入、类型不匹配、缺少必需方法和安全调用 unsafe 函数的精确诊断。`trait-ffi` 通过 `scripts/test/std_crates.csv` 接入 `cargo xtask test`，静态检查入口为 `cargo xtask clippy --package trait-ffi --package trait-ffi-macros`。这些测试验证宏和真实宿主链接，不声称证明内核 IRQ、调度或 DMA 运行时语义。新增导出安全边界在合入前仍需领域审查。
