use std::sync::Arc;

use ctor::ctor;
use scope_local::{ActiveScope, Scope, scope_local};

#[ctor]
fn init() {
    percpu::init();

    unsafe { percpu::write_percpu_reg(percpu::percpu_area_base(0)) };

    let base = percpu::read_percpu_reg();
    println!("per-CPU area base = {base:#x}");
    println!("per-CPU area size = {}", percpu::percpu_area_size());
}

scope_local! {
    static DATA: usize = 0;
}

#[test]
fn global() {
    assert_eq!(*DATA, 0);
}

#[test]
fn scope() {
    let mut scope = Scope::new();
    assert_eq!(*DATA.scope(&scope), 0);

    *DATA.scope_mut(&mut scope) = 42;
    assert_eq!(*DATA.scope(&scope), 42);

    unsafe { ActiveScope::set(&scope) };
    assert_eq!(*DATA, 42);

    ActiveScope::set_global();
}

scope_local! {
    static SHARED: Arc<String> = Arc::new("qwq".to_string());
}

#[test]
fn shared() {
    assert_eq!(Arc::strong_count(&SHARED), 1);

    {
        let mut scope = Scope::new();
        *SHARED.scope_mut(&mut scope) = SHARED.clone();

        assert_eq!(Arc::strong_count(&SHARED), 2);
        assert!(Arc::ptr_eq(&SHARED, &SHARED.scope(&scope)));
    }

    assert_eq!(Arc::strong_count(&SHARED), 1);
}
