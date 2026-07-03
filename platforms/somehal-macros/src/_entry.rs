use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{ItemFn, Visibility, parse, parse_macro_input, spanned::Spanned};

/// Entry 宏参数解析结构
///
/// 必须指定内核类型参数，如 `#[entry(Kernel)]`
struct EntryArgs {
    kernel_type: syn::Ident,
}

impl syn::parse::Parse for EntryArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        // 空参数列表时报错
        if input.is_empty() {
            return Err(syn::Error::new(
                input.span(),
                "this attribute requires a kernel type argument, e.g., `#[entry(Kernel)]`",
            ));
        }

        // 解析类型标识符（如 Kernel）
        let kernel_type = input.parse::<syn::Ident>()?;

        // 确保没有额外参数
        if !input.is_empty() {
            return Err(syn::Error::new(
                input.span(),
                "expected a single type identifier argument",
            ));
        }

        Ok(EntryArgs { kernel_type })
    }
}

pub fn entry(args: TokenStream, input: TokenStream, name: &str) -> TokenStream {
    let f = parse_macro_input!(input as ItemFn);

    // check the function signature
    let valid_signature = f.sig.constness.is_none()
        && f.sig.asyncness.is_none()
        && f.vis == Visibility::Inherited
        && f.sig.abi.is_none()
        && f.sig.generics.params.is_empty()
        && f.sig.generics.where_clause.is_none()
        && f.sig.variadic.is_none()
        // && match f.sig.output {
        //     ReturnType::Default => false,
        //     ReturnType::Type(_, ref ty) => matches!(**ty, Type::Never(_)),
        // }
        ;

    if !valid_signature {
        return parse::Error::new(
            f.span(),
            "`#[entry]` function must have signature `[unsafe] fn([arg0: usize, ...]) `",
        )
        .to_compile_error()
        .into();
    }

    // 解析宏参数
    let entry_args = match syn::parse::<EntryArgs>(args) {
        Ok(args) => args,
        Err(e) => return e.to_compile_error().into(),
    };

    // XXX should we blacklist other attributes?
    let attrs = f.attrs;
    let unsafety = f.sig.unsafety;
    let args = f.sig.inputs;
    let stmts = f.block.stmts;
    let name = format_ident!("{}", name);
    let kernel_type = entry_args.kernel_type;

    // 生成代码：自动插入 somehal::init() 调用
    quote!(
        #[allow(non_snake_case)]
        #[unsafe(no_mangle)]
        #(#attrs)*
        pub #unsafety extern "C" fn #name(#args) {
            somehal::init(&#kernel_type);
            #(#stmts)*
        }
    )
    .into()
}

pub fn entry_secondary(_args: TokenStream, input: TokenStream, is_someboot: bool) -> TokenStream {
    let f = parse_macro_input!(input as ItemFn);

    let name;
    let crate_name;
    if is_someboot {
        name = "__someboot_secondary";
        crate_name = quote!(someboot);
    } else {
        name = "__somehal_secondary";
        crate_name = quote!(somehal);
    }

    // check the function signature
    let valid_signature = f.sig.constness.is_none()
        && f.sig.asyncness.is_none()
        && f.vis == Visibility::Inherited
        && f.sig.abi.is_none()
        && f.sig.generics.params.is_empty()
        && f.sig.generics.where_clause.is_none()
        && f.sig.variadic.is_none()
        // && match f.sig.output {
        //     ReturnType::Default => false,
        //     ReturnType::Type(_, ref ty) => matches!(**ty, Type::Never(_)),
        // }
        ;

    if !valid_signature {
        return parse::Error::new(
            f.span(),
            "`#[entry]` function must have signature `[unsafe] fn([arg0: usize, ...]) `",
        )
        .to_compile_error()
        .into();
    }

    // XXX should we blacklist other attributes?
    let attrs = f.attrs;
    let unsafety = f.sig.unsafety;
    // let args = f.sig.inputs;
    let stmts = f.block.stmts;
    let name = format_ident!("{}", name);

    quote!(
        #[allow(non_snake_case)]
        #[allow(unused_variables)]
        #[unsafe(no_mangle)]
        #(#attrs)*
        pub #unsafety extern "C" fn #name(meta: &#crate_name::smp::PerCpuMeta) {
            #(#stmts)*
        }
    )
    .into()
}
