#[derive(Debug)]
/// The information of guest page walk.
pub struct GuestPageWalkInfo {
    /// The guest page table physical address.
    pub top_entry: usize, // Top level paging structure entry
    /// Guest page table level.
    pub level: usize,
    /// Guest page table width
    pub width: u32,
    /// Guest page table user mode
    pub is_user_mode_access: bool,
    /// Guest page table write access
    pub is_write_access: bool,
    /// Guest page table instruction fetch
    pub is_inst_fetch: bool,
    /// CR4.PSE for 32bit paging, true for PAE/4-level paging
    pub pse: bool,
    /// CR0.WP
    pub wp: bool, // CR0.WP
    /// MSR_IA32_EFER_NXE_BIT
    pub nxe: bool,

    /// Guest page table Supervisor mode access prevention
    pub is_smap_on: bool,
    /// Guest page table Supervisor mode execution protection
    pub is_smep_on: bool,
}
