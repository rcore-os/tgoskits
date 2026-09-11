use std::{panic, sync::Arc};

use scope_local::{Scope, scope_local};

scope_local! {
    static SHARED: Arc<()> = Arc::new(());
}

#[test]
fn scope_owned_resources_drop_during_unwind_without_a_runtime_model() {
    let retained = Arc::new(());
    let panic = panic::catch_unwind(|| {
        let mut scope = Scope::new();
        *SHARED.scope_mut(&mut scope) = Arc::clone(&retained);
        assert_eq!(Arc::strong_count(&retained), 2);
        panic!("release the explicit scope through unwind");
    });

    assert!(panic.is_err());
    assert_eq!(Arc::strong_count(&retained), 1);
}
