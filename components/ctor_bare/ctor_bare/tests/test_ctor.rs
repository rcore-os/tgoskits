use ctor_bare::register_ctor;
use std::sync::{Mutex, atomic::AtomicUsize};

static INIT_NUM: AtomicUsize = AtomicUsize::new(0);

#[register_ctor]
fn set_init_num() {
    INIT_NUM.fetch_add(20, std::sync::atomic::Ordering::Relaxed);
}

static INIT_VEC: Mutex<Vec<usize>> = Mutex::new(Vec::new());

#[register_ctor]
fn init_vector() {
    let mut vec = INIT_VEC.lock().unwrap();
    vec.push(1);
    vec.push(2);
    vec.push(3);
}

#[test]
fn test_ctor_bare() {
    // The constructor functions will be called before the main function.
    assert!(INIT_NUM.load(std::sync::atomic::Ordering::Relaxed) == 20);
    let vec = INIT_VEC.lock().unwrap();
    assert!(vec.len() == 3);
    assert!(vec[0] == 1);
    assert!(vec[1] == 2);
    assert!(vec[2] == 3);
    drop(vec);

    // But we can invoke the constructor functions again manually.
    init_vector();
    let vec = INIT_VEC.lock().unwrap();
    assert!(vec.len() == 6);
    assert!(vec[3] == 1);
    assert!(vec[4] == 2);
    assert!(vec[5] == 3);
}
