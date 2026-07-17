#[test]
fn ready_block_device_requires_initialized_card_capability() {
    let device = include_str!("../src/rdif/device.rs");
    let staged = include_str!("../src/rdif/staged.rs");
    let owned_init = include_str!("../src/sdio/owned_init.rs");

    assert!(device.contains("pub fn from_initialized("));
    assert!(!device.contains("pub fn new(mut card: SdioSdmmc"));
    assert!(staged.contains("fn(InitializedSdioCard<H>, BlockConfig)"));
    assert!(owned_init.contains("Result<InitializedSdioCard<H>, Box<Self>>"));
}
