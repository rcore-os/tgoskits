use proc_macro::TokenStream;

mod _entry;
mod trap;

/// Attribute to declare the entry point of the program
///
/// **IMPORTANT**: This attribute must appear exactly *once* in the dependency graph. Also, if you
/// are using Rust 1.30 the attribute must be used on a reachable item (i.e. there must be no
/// private modules between the item and the root of the crate); if the item is in the root of the
/// crate you'll be fine. This reachability restriction doesn't apply to Rust 1.31 and newer releases.
///
/// The specified function will be called by the reset handler *after* RAM has been initialized.
/// If present, the FPU will also be enabled before the function is called.
///
/// The type of the specified function must be `[unsafe] fn() -> !` (never ending function)
///
/// # 属性参数
///
/// 此宏**必需**接受一个内核类型参数：
///
/// - **参数**: 指定实现 `somehal::KernelOp` trait 的类型标识符
/// - **行为**: 自动在函数开头插入 `somehal::init(&<type>)` 调用
///
/// # Properties
///
/// The entry point will be called by the reset handler. The program can't reference to the entry
/// point, much less invoke it.
///
/// # Examples
///
/// - Entry point with kernel initialization
///
/// ``` no_run
/// # #![no_main]
/// # use pie_boot::entry;
/// # struct Kernel;
/// # impl somehal::KernelOp for Kernel {
/// #     fn ioremap(&self, paddr: usize, size: usize) -> somehal::PagingResult<*mut u8> {
/// #         Ok(std::ptr::null_mut())
/// #     }
/// # }
/// #[entry(Kernel)]
/// fn main() -> ! {
///     // somehal::init(&Kernel) 已自动生成
///     loop { /* .. */ }
/// }
/// ```
#[proc_macro_attribute]
pub fn entry(args: TokenStream, input: TokenStream) -> TokenStream {
    _entry::entry(args, input, "__someboot_main")
}

/// Attribute to declare the secondary entry point of the program
///
/// # Examples
///
/// - Simple entry point
///
/// ``` no_run
/// # #![no_main]
/// # use pie_boot::secondary_entry;
/// #[entry]
/// fn secondary(cpu_id: usize) -> ! {
///     loop { /* .. */ }
/// }
/// ```
#[proc_macro_attribute]
pub fn someboot_secondary_entry(args: TokenStream, input: TokenStream) -> TokenStream {
    _entry::entry_secondary(args, input, true)
}

/// Attribute to declare the secondary entry point of the program
///
/// # Examples
///
/// - Simple entry point
///
/// ``` no_run
/// # #![no_main]
/// # use pie_boot::secondary_entry;
/// #[entry]
/// fn secondary(cpu_id: usize) -> ! {
///     loop { /* .. */ }
/// }
/// ```
#[proc_macro_attribute]
pub fn somehal_secondary_entry(args: TokenStream, input: TokenStream) -> TokenStream {
    _entry::entry_secondary(args, input, false)
}

/// 中断处理器属性宏
///
/// 将用户函数转换为标准中断处理器，自动生成正确的函数签名。
///
/// # 要求
///
/// 函数必须：
/// - 不能是 `const`、`async` 或有泛型参数
/// - 必须有且仅有一个参数，类型为 `someboot::irq::IrqId`
/// - 无返回类型
/// - 不应有显式的可见性声明（宏自动设为 `pub`）
///
/// # 示例
///
/// ```
/// # #![no_std]
/// # use somehal_macros::irq_handler;
/// # use someboot::irq::IrqId;
/// #[irq_handler]
/// fn my_irq_handler(irq: IrqId) {
///     // 处理中断
///     sparreal_kernel::os::irq::handle_irq(irq);
/// }
/// ```
///
/// # 生成代码
///
/// 宏展开为：
///
/// ```ignore
/// #[unsafe(no_mangle)]
/// pub extern "Rust" fn _someboot_handle_irq(irq: someboot::irq::IrqId) {
///     // 你的代码
/// }
/// ```
///
/// # 平台集成
///
/// 该函数由 HAL 的 trap 处理代码自动调用：
///
/// - AArch64: `crates/somehal/src/arch/aarch64/trap.rs`
/// - LoongArch64: `crates/somehal/src/arch/loongarch64/trap.rs`
///
/// 中断号随后通过 `sparreal_kernel::os::irq::handle_irq()` 分发到注册的处理函数。
///
/// # 注意
///
/// - 每个平台只能有一个全局中断入口（符号名固定为 `_someboot_handle_irq`）
/// - 参数名会被保留（如 `irq`、`hwirq` 等），不会被强制修改
#[proc_macro_attribute]
pub fn irq_handler(args: TokenStream, input: TokenStream) -> TokenStream {
    trap::irq_handler(args, input)
}
