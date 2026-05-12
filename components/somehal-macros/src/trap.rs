use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use syn::{FnArg, ItemFn, ReturnType, Type, Visibility, parse_macro_input, spanned::Spanned};

pub fn irq_handler(args: TokenStream, input: TokenStream) -> TokenStream {
    let f = parse_macro_input!(input as ItemFn);

    // 1. 检查宏参数（应该为空）
    if !args.is_empty() {
        return syn::Error::new(
            Span::call_site(),
            "`#[irq_handler]` does not accept any arguments",
        )
        .to_compile_error()
        .into();
    }

    // 2. 验证函数签名
    if let Some(err) = validate_signature(&f) {
        return err.to_compile_error().into();
    }

    // 3. 验证参数
    let input_arg = match validate_args(&f.sig.inputs) {
        Ok(arg) => arg,
        Err(err) => return err.to_compile_error().into(),
    };

    // 4. 验证返回类型
    if let Some(err) = validate_return_type(&f.sig.output) {
        return err.to_compile_error().into();
    }

    // 5. 提取函数组件
    let attrs = f.attrs;
    let unsafety = f.sig.unsafety;
    let stmts = f.block.stmts;

    // 6. 生成标准中断处理器
    let expanded = quote! {
        #[unsafe(no_mangle)]
        #(#attrs)*
        pub #unsafety extern "Rust" fn _someboot_handle_irq(#input_arg) {
            #(#stmts)*
        }
    };

    expanded.into()
}

/// 验证函数签名基本要求
fn validate_signature(f: &ItemFn) -> Option<syn::Error> {
    // 检查 const
    if f.sig.constness.is_some() {
        return Some(syn::Error::new(
            f.span(),
            "`#[irq_handler]` function cannot be const",
        ));
    }

    // 检查 async
    if f.sig.asyncness.is_some() {
        return Some(syn::Error::new(
            f.span(),
            "`#[irq_handler]` function cannot be async",
        ));
    }

    // 检查泛型
    if !f.sig.generics.params.is_empty() || f.sig.generics.where_clause.is_some() {
        return Some(syn::Error::new(
            f.sig.generics.span(),
            "`#[irq_handler]` function cannot have generic parameters",
        ));
    }

    // 检查变参
    if f.sig.variadic.is_some() {
        return Some(syn::Error::new(
            f.sig.variadic.span(),
            "`#[irq_handler]` function cannot be variadic",
        ));
    }

    // 检查可见性（应该是 private，宏会强制为 pub）
    if !matches!(f.vis, Visibility::Inherited) {
        return Some(syn::Error::new(
            f.span(),
            "`#[irq_handler]` function should not have explicit visibility",
        ));
    }

    None
}

/// 验证参数列表（必须有且仅有一个 IrqId 参数）
fn validate_args(
    inputs: &syn::punctuated::Punctuated<FnArg, syn::token::Comma>,
) -> Result<&FnArg, syn::Error> {
    match inputs.len() {
        0 => Err(syn::Error::new(
            inputs.span(),
            "`#[irq_handler]` function must have exactly one `IrqId` parameter",
        )),
        1 => {
            match inputs.first().unwrap() {
                FnArg::Typed(arg) => {
                    // 验证参数类型
                    if !is_irq_id_type(&arg.ty) {
                        return Err(syn::Error::new(
                            arg.ty.span(),
                            "`#[irq_handler]` argument type must be `someboot::irq::IrqId`",
                        ));
                    }
                    Ok(inputs.first().unwrap())
                }
                FnArg::Receiver(receiver) => Err(syn::Error::new(
                    receiver.span(),
                    "`#[irq_handler]` function cannot have `self` parameter",
                )),
            }
        }
        _ => Err(syn::Error::new(
            inputs.span(),
            "`#[irq_handler]` function must have exactly one `IrqId` parameter",
        )),
    }
}

/// 检查是否是 IrqId 类型
fn is_irq_id_type(ty: &Type) -> bool {
    match ty {
        Type::Path(path) => {
            let type_str = quote!(#path).to_string();
            // 匹配: IrqId, somehal::irq::IrqId, crate::irq::IrqId 等
            type_str.ends_with("IrqId") || type_str.contains("irq::IrqId")
        }
        _ => false,
    }
}

/// 验证返回类型（必须无返回值）
fn validate_return_type(ret: &ReturnType) -> Option<syn::Error> {
    match ret {
        ReturnType::Default => None,
        ReturnType::Type(_, ty) => Some(syn::Error::new(
            ty.span(),
            "`#[irq_handler]` function must not have a return type",
        )),
    }
}
