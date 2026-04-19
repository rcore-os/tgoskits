struct ArchIfImpl;

const _: ArchIfImpl = ArchIfImpl;

#[axvisor_api::api_impl]
impl axvisor_api::arch::ArchIf for ArchIfImpl {}
