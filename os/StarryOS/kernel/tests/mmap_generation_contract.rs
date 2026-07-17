//! File-backed mappings must retain behavior leases without pinning them in caches.

const COW_BACKEND: &str = include_str!("../src/mm/aspace/backend/cow.rs");
const ELF_LOADER: &str = include_str!("../src/mm/loader.rs");
const FILE_BACKEND: &str = include_str!("../src/mm/aspace/backend/file.rs");

#[test]
fn elf_parse_cache_keeps_only_a_lease_free_generation_identity() {
    let entry = type_body(ELF_LOADER, "struct ElfCacheEntry");

    assert!(entry.contains("location: FileLocation"));
    assert!(!entry.contains("cache: CachedFile"));
}

#[test]
fn every_elf_mapping_promotes_the_cached_identity_to_a_live_behavior_handle() {
    let load = function_body(ELF_LOADER, "fn load(&mut self,");
    let map = function_body(ELF_LOADER, "fn map_elf<");

    assert!(load.contains("open_cached_location"));
    assert!(ELF_LOADER.contains("entry: &'a ElfCacheEntry,\n    cache: &CachedFile,"));
    assert!(map.contains("FileBackend::Cached(cache.clone())"));
}

#[test]
fn mapped_backends_retain_the_promoted_file_behavior_until_unmap() {
    assert!(FILE_BACKEND.contains("cache: CachedFile"));
    assert!(COW_BACKEND.contains("file: Option<(FileBackend,"));

    let shared_clone = function_body(FILE_BACKEND, "pub fn with_start(");
    assert!(shared_clone.contains("cache: self.0.cache.clone()"));
    let private_clone = trait_body(COW_BACKEND, "impl Clone for CowBackend");
    assert!(private_clone.contains("file: self.file.clone()"));
}

#[test]
fn cache_listeners_cannot_keep_a_mapping_lease_alive() {
    let listener = function_body(FILE_BACKEND, "pub fn register_listener(");

    assert!(listener.contains("Arc::downgrade(self)"));
    assert!(!listener.contains("let this = self.clone()"));
    assert!(FILE_BACKEND.contains("impl Drop for FileBackendInner"));
    assert!(FILE_BACKEND.contains("remove_evict_listener(handle)"));
}

#[test]
fn old_generation_faults_propagate_backend_errors_instead_of_using_stale_bytes() {
    let fault = function_body(COW_BACKEND, "fn alloc_new_at(");

    assert!(fault.contains("file.read_at("));
    assert!(fault.contains("return Err(err)"));
    assert!(!fault.contains("unwrap_or_default"));
    assert!(!fault.contains("unwrap_or(0)"));
}

fn function_body<'source>(source: &'source str, signature: &str) -> &'source str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing function `{signature}`"));
    braced_body(&source[start..], signature)
}

fn type_body<'source>(source: &'source str, signature: &str) -> &'source str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing type `{signature}`"));
    braced_body(&source[start..], signature)
}

fn trait_body<'source>(source: &'source str, signature: &str) -> &'source str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing implementation `{signature}`"));
    braced_body(&source[start..], signature)
}

fn braced_body<'source>(source: &'source str, context: &str) -> &'source str {
    let open = source
        .find('{')
        .unwrap_or_else(|| panic!("missing body for `{context}`"));
    let mut depth = 0_usize;
    for (offset, character) in source[open..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[open..=open + offset];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated body for `{context}`")
}
