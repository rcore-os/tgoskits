use proc_macro::TokenStream;

mod _entry;

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
/// ```ignore
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
/// ```ignore
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
/// ```ignore
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
