//! Published Starry page tables must mutate only inside the mm-owned gather transaction.

const ASPACE: &str = include_str!("../src/mm/aspace/mod.rs");
const COW_BACKEND: &str = include_str!("../src/mm/aspace/backend/cow.rs");
const FILE_BACKEND: &str = include_str!("../src/mm/aspace/backend/file.rs");
const MEMFD: &str = include_str!("../src/file/memfd.rs");
const TLB: &str = include_str!("../src/mm/aspace/tlb.rs");

#[test]
fn published_page_table_has_no_public_mutable_escape_hatch() {
    assert!(ASPACE.contains("fn page_table_mut(&mut self) -> &mut PageTable"));
    assert!(!ASPACE.contains("pub fn page_table_mut(&mut self) -> &mut PageTable"));
    assert!(!ASPACE.contains("pub(crate) fn page_table_mut(&mut self) -> &mut PageTable"));

    let mutation = function_body(ASPACE, "fn publish_tlb_gather_mutation<R>(");
    assert!(mutation.contains("tlb_quarantine"));
    assert!(mutation.contains("TlbGather::new()"));
    assert!(mutation.contains(".commit(gather,"));
}

#[test]
fn dirty_write_protection_uses_the_address_space_transaction() {
    let write_protect = function_body(FILE_BACKEND, "fn protect_dirty_page(");
    assert!(write_protect.contains("aspace.mutate_with_tlb_gather_confirmed"));
    assert!(write_protect.contains("gather.record_range"));
    assert!(!write_protect.contains("flush_tlb_range"));
}

#[test]
fn file_cache_release_requires_confirmed_remote_invalidation() {
    let evict = function_body(FILE_BACKEND, "fn on_evict(");
    assert!(evict.contains("aspace.mutate_with_tlb_gather_confirmed"));
    assert!(!evict.contains("aspace.mutate_with_tlb_gather(&[]"));
}

#[test]
fn failed_teardown_is_transferred_to_an_owner_quarantine() {
    assert!(ASPACE.contains("struct RetainedAddressSpaceNode"));
    assert!(ASPACE.contains("static RETAINED_ADDRESS_SPACES"));
    assert!(ASPACE.contains("retain_failed_address_space_teardown"));

    let drop_impl = function_body(ASPACE, "fn drop(&mut self)");
    assert!(!drop_impl.contains("self.clear();"));
    assert!(!drop_impl.contains("retry_quarantined_tlb_reclaims"));
}

#[test]
fn file_eviction_ownership_moves_before_fallible_page_mapping() {
    let populate = function_body(FILE_BACKEND, "fn populate(");
    let retain = populate
        .find("gather.retain_file_page(")
        .expect("file eviction must transfer frame ownership into the gather");
    let physical_address = populate
        .find(".paddr()")
        .expect("file population must resolve a physical address");
    let page_table_map = populate
        .find("pt.map_page(")
        .expect("file population must install the page-table entry");

    assert!(retain < physical_address);
    assert!(retain < page_table_map);
    let reserve = populate
        .find(".prepare_file_page_retention()")
        .expect("file eviction retention capacity must be reserved before cache eviction");
    let cache_insert = populate
        .find("with_page_or_insert_excluding_owner")
        .expect("file populate must exclude only its own address-space listeners");
    assert!(reserve < cache_insert);
    assert!(!populate.contains("to_be_evicted"));
    assert!(ASPACE.contains("self.finish_retained_file_evictions(&mut gather);"));

    let finish = function_body(ASPACE, "fn finish_retained_file_evictions(");
    assert!(finish.contains("take_retained_file_evictions"));
    assert!(!finish.contains("collect::<"));
    assert!(!finish.contains("let owners: alloc::vec::Vec"));
}

#[test]
fn post_pte_tlb_bookkeeping_is_prepared_or_infallible() {
    let publish = function_body(ASPACE, "fn publish_tlb_gather_mutation<R>(");
    let reserve = publish
        .find(".prepare_ranges(ranges.len())")
        .expect("range capacity must be prepared before the operation");
    let operation = publish
        .find("let operation_result = operation(")
        .expect("missing page-table operation");
    assert!(reserve < operation);

    assert!(TLB.contains("conservatively collapse to one larger invalidation range"));
    assert!(TLB.contains("prepare_deferred_frames"));
    assert!(COW_BACKEND.contains(".prepare_deferred_frames(1)"));
    assert!(COW_BACKEND.contains(".prepare_deferred_frames(mapped.len())"));
}

#[test]
fn address_space_clear_commits_memfd_accounting_after_vma_removal() {
    let clear = function_body(ASPACE, "fn clear_without_retained_retry(&mut self)");
    let prepare = clear
        .find("memfd_prepare_shared_writable_release")
        .expect("memfd accounting release must be prepared before page-table mutation");
    let mutation = clear
        .find("publish_tlb_gather_mutation")
        .expect("address-space clear must use a TLB gather transaction");
    let remove_areas = clear
        .find("aspace.areas.clear")
        .expect("address-space clear must remove every VMA");
    let commit = clear
        .find("memfd_release.commit()")
        .expect("memfd accounting release must have an explicit commit point");

    assert!(prepare < mutation);
    assert!(remove_areas < commit);
    assert!(!clear.contains("memfd_release_all_shared_writable_counts_for_aspace"));
    assert!(MEMFD.contains("struct SharedWritableRelease"));
    assert!(MEMFD.contains("pub(crate) fn commit(self)"));
}

#[test]
fn unmap_commits_memfd_accounting_after_the_vma_transaction() {
    let unmap = function_body(ASPACE, "fn unmap_inner(");
    let prepare = unmap
        .find("memfd_prepare_shared_writable_unmap")
        .expect("memfd accounting must be prepared before the VMA transaction");
    let mutation = unmap
        .find("self.areas.unmap")
        .expect("unmap must commit the VMA transaction");
    let commit = unmap
        .find("memfd_update.commit()")
        .expect("memfd accounting needs an explicit post-VMA commit");

    assert!(prepare < mutation);
    assert!(mutation < commit);
    assert!(!unmap.contains("memfd_on_aspace_unmap_range"));
}

#[test]
fn moved_source_backends_survive_unconfirmed_tlb_invalidation() {
    let move_pages = function_body(ASPACE, "pub fn move_pages(");
    let retain = move_pages
        .find("retain_backends_for_range")
        .expect("mremap must retain every source backend in its TLB gather");
    let pte_move = move_pages
        .find("move_pages_inner")
        .expect("mremap must relocate the source PTEs");

    assert!(retain < pte_move);
}

fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing source item: {signature}"));
    let source = &source[start..];
    let brace = source
        .find('{')
        .unwrap_or_else(|| panic!("missing source body: {signature}"));
    let mut depth = 0usize;
    for (offset, character) in source[brace..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[..brace + offset + character.len_utf8()];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated source item: {signature}");
}
