#[test]
fn test_empty() {
    // Sometimes under certain conditions, we may not have any constructor functions.
    // But the `call_ctors` function should still work, and the `__init_array_start` and
    // `__init_array_end` symbols should be valid.
    ctor_bare::call_ctors();
    println!("It should exit successfully when we don't specify any constructor functions.");
}
