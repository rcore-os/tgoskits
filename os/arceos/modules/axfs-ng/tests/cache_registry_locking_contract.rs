//! Lock-order contracts for the global cached-file registry.

const SHARED_CACHE_SOURCE: &str = include_str!("../src/file/cache/shared.rs");

#[test]
fn cache_registration_does_not_query_sleepable_cache_state_under_registry_spinlock() {
    let body = function_body(SHARED_CACHE_SOURCE, "pub(super) fn register_cached_file(");

    assert!(
        !body.contains("has_dirty_pages"),
        "cache registration must only mutate registry membership while its spin lock is held"
    );
}

#[test]
fn global_sync_does_not_query_sleepable_cache_state_under_registry_spinlock() {
    let body = function_body(SHARED_CACHE_SOURCE, "pub fn sync_all_cached_files(");

    assert!(
        !body.contains(
            "guard.retain(|cached| Arc::strong_count(cached) > 1 || cached.has_dirty_pages())"
        ),
        "global sync must detach registry entries before inspecting their PI-protected cache state"
    );
}

#[test]
fn allocator_reclaim_does_not_run_cache_callbacks_under_registry_spinlock() {
    let body = function_body(SHARED_CACHE_SOURCE, "pub fn page_cache_reclaim(");

    assert!(
        !body.contains("for file in guard.iter()"),
        "allocator reclaim must pin a bounded batch, release the registry lock, and only then \
         inspect caches"
    );
}

fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing function `{signature}`"));
    let open = source[start..]
        .find('{')
        .map(|offset| start + offset)
        .unwrap_or_else(|| panic!("missing body for `{signature}`"));
    let mut depth = 0usize;
    for (offset, byte) in source.as_bytes()[open..].iter().copied().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[open + 1..open + offset];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated body for `{signature}`")
}
