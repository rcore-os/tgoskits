#![doc = include_str!("../README.md")]
#![no_std]

/// Define an interface.
///
/// This attribute should be added above the definition of a trait. All traits
/// that use the attribute cannot have the same name.
///
/// It is not necessary to define it in the same crate as the implementation,
/// but it is required that these crates are linked together.
///
/// See the [crate-level documentation](crate) for more details.
#[macro_export]
macro_rules! def_interface {
    ($vis:vis trait $name:ident {$(
        $(#[$fn_attr:meta])*
        fn $fn_name:ident($($arg_name:ident: $arg_ty:ty),* $(,)?) $(-> $ret_ty:ty)?;
    )*}) => {
        $vis trait $name {$(
            $(#[$fn_attr])*
            fn $fn_name($($arg_name: $arg_ty,)*) $(-> $ret_ty)?;
        )*}

        impl $name for $crate::r#priv::DefaultImpl {$(
            $(#[$fn_attr])*
            fn $fn_name($($arg_name: $arg_ty,)*) $(-> $ret_ty)? {
                extern "Rust" {
                    #[link_name = concat!("__", stringify!($name), "__", stringify!($fn_name))]
                    fn $fn_name($($arg_name: $arg_ty,)*) $(-> $ret_ty)?;
                }
                unsafe { $fn_name($($arg_name,)*) }
            }
        )*}
    };
}

/// Implement the interface for a struct.
///
/// This attribute should be added above the implementation of a trait for a
/// struct, and the trait must be defined with [`def_interface!`].
///
/// It is not necessary to implement it in the same crate as the definition, but
/// it is required that these crates are linked together.
///
/// See the [crate-level documentation](crate) for more details.
#[macro_export]
macro_rules! impl_interface {
    (impl $interface:ident for $target:ident {$(
        $(#[$fn_attr:meta])*
        fn $fn_name:ident($($arg_name:ident: $arg_ty:ty),* $(,)?) $(-> $ret_ty:ty)? { $($body:tt)* }
    )*}) => {
        impl $interface for $target {$(
            $(#[$fn_attr])*
            fn $fn_name($($arg_name: $arg_ty,)*) $(-> $ret_ty)? {
                #[export_name = concat!("__", stringify!($interface), "__", stringify!($fn_name))]
                extern "Rust" fn $fn_name($($arg_name: $arg_ty,)*) $(-> $ret_ty)? {
                    $($body)*
                }
                $fn_name($($arg_name,)*)
            }
        )*}
    };
}

/// Call a function in the interface.
///
/// It is not necessary to call it in the same crate as the implementation, but
/// it is required that these crates are linked together.
///
/// See the [crate-level documentation](crate) for more details.
#[macro_export]
macro_rules! call_interface {
    ($($path:ident)::+ $(, $args:expr)* $(,)?) => {
        ($crate::__interface_fn!([] $($path)::*))($($args,)*)
    };
    ($($path:ident)::+ ($($args:tt)*) $(,)?) => {
        ($crate::__interface_fn!([] $($path)::*))($($args)*)
    };
    (::$($path:ident)::+ $(, $args:expr)* $(,)?) => {
        ($crate::__interface_fn!([::] $($path)::*))($($args,)*)
    };
    (::$($path:ident)::+ ($($args:tt)*) $(,)?) => {
        ($crate::__interface_fn!([::] $($path)::*))($($args)*)
    };
}

/// Converts the given path to the default interface implementation.
#[doc(hidden)]
#[macro_export]
macro_rules! __interface_fn {
    ([$($path:tt)*] $interface:ident::$fn_name:ident) => {
        <$crate::r#priv::DefaultImpl as $($path)*$interface>::$fn_name
    };
    ([$($path:tt)*] $head:ident::$($rest:tt)+) => {
        $crate::__interface_fn!([$($path)* $head::] $($rest)*)
    };
}

/// NON-PUBLIC APIs
pub mod r#priv {
    pub struct DefaultImpl;
}
