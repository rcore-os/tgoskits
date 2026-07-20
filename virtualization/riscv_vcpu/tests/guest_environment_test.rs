#[path = "../src/environment.rs"]
mod environment;

use environment::RiscvGuestIsaConfig;
use riscv_h::register::henvcfg::CacheBlockInvalidate;

#[test]
fn inherited_host_isa_enables_cache_block_operations() {
    let henvcfg = RiscvGuestIsaConfig::inherited_host().henvcfg();

    assert_eq!(
        henvcfg.cache_block_invalidate(),
        CacheBlockInvalidate::Invalidate
    );
    assert!(henvcfg.cache_block_clean_flush());
    assert!(henvcfg.cache_block_zero());
}

#[test]
fn baseline_isa_keeps_cache_block_operations_trapped() {
    let henvcfg = RiscvGuestIsaConfig::baseline().henvcfg();

    assert_eq!(henvcfg.cache_block_invalidate(), CacheBlockInvalidate::Trap);
    assert!(!henvcfg.cache_block_clean_flush());
    assert!(!henvcfg.cache_block_zero());
}

#[test]
fn individual_extensions_only_enable_their_own_environment_controls() {
    let zicbom = RiscvGuestIsaConfig::baseline().with_zicbom().henvcfg();
    assert_eq!(
        zicbom.cache_block_invalidate(),
        CacheBlockInvalidate::Invalidate
    );
    assert!(zicbom.cache_block_clean_flush());
    assert!(!zicbom.cache_block_zero());

    let zicboz = RiscvGuestIsaConfig::baseline().with_zicboz().henvcfg();
    assert_eq!(zicboz.cache_block_invalidate(), CacheBlockInvalidate::Trap);
    assert!(!zicboz.cache_block_clean_flush());
    assert!(zicboz.cache_block_zero());
}
