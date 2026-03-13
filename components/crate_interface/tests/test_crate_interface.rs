use crate_interface::*;

#[def_interface]
trait SimpleIf {
    fn foo() -> u32;

    /// Test comments
    fn bar(a: u16, b: &[u8], c: &str);
}

struct SimpleIfImpl;

#[impl_interface]
impl SimpleIf for SimpleIfImpl {
    #[cfg(test)]
    fn foo() -> u32 {
        456
    }

    /// Test comments2
    fn bar(a: u16, b: &[u8], c: &str) {
        println!("{} {:?} {}", a, b, c);
        assert_eq!(b[1], 3);
    }
}

#[def_interface(gen_caller)]
trait WithCallerIf {
    fn baz(x: i32) -> i32;
}

struct WithCallerIfImpl;

#[impl_interface]
impl WithCallerIf for WithCallerIfImpl {
    fn baz(x: i32) -> i32 {
        x + 1
    }
}

mod a {
    #[crate_interface::def_interface(gen_caller, namespace = A_NS)]
    pub trait NamespaceIf {
        fn qux() -> i32;
    }
}

mod b {
    #[crate_interface::def_interface(gen_caller, namespace = B_NS)]
    pub trait NamespaceIf {
        fn qux() -> i32;
    }
}

struct NamespaceIfImplA;
struct NamespaceIfImplB;

#[crate_interface::impl_interface(namespace = A_NS)]
impl a::NamespaceIf for NamespaceIfImplA {
    fn qux() -> i32 {
        1
    }
}

#[crate_interface::impl_interface(namespace = B_NS)]
impl b::NamespaceIf for NamespaceIfImplB {
    fn qux() -> i32 {
        2
    }
}

mod private {
    pub fn test_call_in_mod() {
        crate::call_interface!(super::SimpleIf::bar(123, &[2, 3, 5, 7, 11], "test"));
        crate::call_interface!(crate::SimpleIf::foo,);
    }
}

#[test]
fn test_crate_interface_call() {
    call_interface!(SimpleIf::bar, 123, &[2, 3, 5, 7, 11], "test");
    assert_eq!(call_interface!(SimpleIf::foo), 456);
    private::test_call_in_mod();
}

#[test]
fn test_calling_helper_function() {
    assert_eq!(baz(42), 43);
}

#[test]
fn test_namespace_interface() {
    assert_eq!(call_interface!(namespace = A_NS, a::NamespaceIf::qux), 1);
    assert_eq!(call_interface!(namespace = B_NS, b::NamespaceIf::qux), 2);

    assert_eq!(a::qux(), 1);
    assert_eq!(b::qux(), 2);
}
