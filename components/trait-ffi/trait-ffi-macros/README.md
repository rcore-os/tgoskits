# trait-ffi 宏实现

本软件包在编译器宿主端运行，使用工作区 `syn 3` 为 `trait-ffi` 生成静态接口。原始代码来自 `ZR233/trait-ffi` 提交 `61379b7043341dced18b03245cc85a46aef85a33`，保留 MIT 许可及原作者。

## 1. 生成边界

`def_extern_trait` 解析接口后调用 `definition::expand`，由同一份声明生成调用函数和提供者导出；调用者应通过公共包使用它。

### 1.1 选项与身份

`args::Args` 严格解析属性参数，`args::link_prefix` 根据定义包及 trait 身份隔离符号，避免接口和版本之间串用。

### 1.2 签名与导出

`signature::validate` 限定受支持的关联函数；`signature::export_signature` 保留类型和借用关系。`definition::expand` 通过 `attributes::availability` 在定义包中选择 cfg/cfg_attr 分支，生成作用于唯一提供者的实现宏。`implementation::expand` 与 `call::expand` 从 trait 路径找到同一入口，不重新推导 ABI 或符号名。`weak::expand` 只为显式选择弱默认的安全 trait 生成无提供者时的默认导出。

## 2. 验证入口

宏的公共契约测试位于同目录维护的 `trait-ffi` 包，覆盖真实跨包链接、默认方法、重命名依赖及编译诊断。完整使用说明与安全边界见[公共包文档](../trait-ffi/README.md)。静态检查使用 `cargo xtask clippy --package trait-ffi-macros`，契约测试随 `cargo xtask test` 运行。
