#[cfg(any(target_arch = "loongarch64", target_arch = "x86_64"))]
use core::arch::naked_asm;
use core::{
    ffi::c_void,
    fmt::Write,
    mem::{MaybeUninit, align_of, size_of},
    ptr::{addr_of_mut, null},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

pub use uefi::Status;
use uefi::{
    Guid, Result,
    boot::{self, AllocateType, MemoryDescriptor, MemoryType},
    guid,
    prelude::*,
    proto::{
        device_path::build,
        loaded_image::LoadedImage,
        media::{
            file::{File, FileAttribute, FileInfo, FileMode, FileType, RegularFile},
            load_file::LoadFile2,
        },
        rng::Rng,
    },
    runtime,
    system::with_config_table,
    table::{self, cfg::ConfigTableEntry},
};

#[cfg(target_arch = "x86_64")]
use crate::arch::relocate;
#[cfg(target_arch = "loongarch64")]
use crate::arch::try_relocate as relocate;
#[cfg(not(target_arch = "riscv64"))]
use crate::{ArchTrait, arch::Arch};
use crate::{
    acpi::set_rsdp,
    efi_memory_map::{
        AlignedBytes, EXIT_BOOT_MEMORY_MAP_BUFFER_SIZE, RawBootMemoryMap, read_exit_boot_memory_map,
    },
    mem::{__io, __va},
};

cfg_select! {
    target_arch = "loongarch64" => {
        pub(crate) mod loongarch64;
        use loongarch64 as platform;
        pub use loongarch64::{ResetType, reset};
    }
    any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    ) => {
        mod firmware;
        use firmware as platform;
    }
}

const EFI_IMAGE_HANDLE_UNSET: usize = usize::MAX;
const EXIT_BOOT_MEMORY_MAP_DESCRIPTOR_CAPACITY: usize =
    EXIT_BOOT_MEMORY_MAP_BUFFER_SIZE / size_of::<MemoryDescriptor>();
const EXIT_BOOT_MEMORY_MAP_RETRIES: usize = 3;
pub(crate) const FDT_TABLE_GUID: Guid = guid!("b1b621d5-f19c-41a5-830b-d9152c69aae0");
const LINUX_EFI_INITRD_MEDIA_GUID: Guid = guid!("5568e427-68fc-4f3d-ac74-ca555231cc68");
const LINUX_INITRD_DEVICE_PATH_SIZE: usize = 24;
const MAX_UEFI_INITRAMFS_BYTES: usize = 1024 * 1024 * 1024;
use crate::handoff::{MAX_CMDLINE, cmdline::decode_efi_cmdline};

const _: () =
    assert!(size_of::<LoadFile2>() == size_of::<uefi_raw::protocol::media::LoadFile2Protocol>());
const _: () =
    assert!(align_of::<LoadFile2>() == align_of::<uefi_raw::protocol::media::LoadFile2Protocol>());

static mut EXIT_BOOT_MEMORY_MAP_BUFFER: AlignedBytes<EXIT_BOOT_MEMORY_MAP_BUFFER_SIZE> =
    AlignedBytes([0; EXIT_BOOT_MEMORY_MAP_BUFFER_SIZE]);
static mut EXIT_BOOT_MEMORY_MAP_DESCRIPTORS: [MaybeUninit<MemoryDescriptor>;
    EXIT_BOOT_MEMORY_MAP_DESCRIPTOR_CAPACITY] =
    [const { MaybeUninit::uninit() }; EXIT_BOOT_MEMORY_MAP_DESCRIPTOR_CAPACITY];
static EFI_IMAGE_HANDLE: AtomicUsize = AtomicUsize::new(EFI_IMAGE_HANDLE_UNSET);
#[cfg(target_arch = "loongarch64")]
unsafe extern "C" {
    fn __bss_start();
    fn __bss_stop();
}

pub(crate) fn setup_service(system_table: *const ::core::ffi::c_void) {
    let system_table = platform::normalize_system_table(system_table);
    unsafe { table::set_system_table(system_table.cast()) };
    let inherited_handoff = platform::prepare_service_context(system_table);
    if let Some(image_handle) = saved_image_handle() {
        unsafe { boot::set_image_handle(image_handle) };
    }
    if !inherited_handoff {
        setup_console();
        println!("UEFI console ok.");
    }
    find_fdt();
    find_acpi_rsdp();
}

pub(crate) mod memmap;
pub mod pe;

/// EFI PE 入口点 - 符合 EFI ABI 的汇编包装
/// 参数: a0 = image_handle, a1 = system_table
#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text")]
pub unsafe extern "C" fn __x86_64_efi_pe_entry() -> Status {
    naked_asm!(
        "sub rsp, 8",
        "mov r12, rcx",
        "mov r13, rdx",
        "call {relocate}",
        "mov rdi, r12",
        "mov rsi, r13",
        "add rsp, 8",
        "jmp {entry}",
        relocate = sym relocate,
        entry = sym efi_pe_entry_main,
    )
}

#[cfg(not(target_arch = "riscv64"))]
unsafe extern "C" fn efi_pe_entry_main(
    image_handle: Handle,
    system_table: *const ::core::ffi::c_void,
) -> Status {
    unsafe {
        boot::set_image_handle(image_handle);
        table::set_system_table(system_table.cast());
        prepare_entry_payload(image_handle);
        // Safety: `system_table` comes from the EFI firmware entry path and
        // matches the contract documented on `ArchTrait::efi_enter_kernel`.
        if Arch::efi_enter_kernel(system_table) {
            Status::SUCCESS
        } else {
            unreachable!()
        }
    }
}

pub(crate) fn prepare_entry_payload(image_handle: Handle) {
    save_image_handle(image_handle);
    platform::prepare_entry_payload(image_handle);
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn discard_entry_payload() {
    if let Some((start, end)) = crate::boot_payload::take_staged_uefi() {
        let address = core::ptr::NonNull::new(start as *mut u8)
            .expect("staged initramfs has an EFI allocation");
        let pages = (end - start).div_ceil(crate::consts::PAGE_SIZE);
        // SAFETY: the staged allocation has not been published and still
        // owns precisely these firmware pages. Boot Services remain active.
        unsafe { boot::free_pages(address, pages) }.expect("failed to release rejected initramfs");
    }
    #[cfg(feature = "boot-resources")]
    // SAFETY: the EFI entry is rejecting the handoff before publishing the
    // boot CPU record, starting other CPUs or taking over Boot Services.
    unsafe {
        crate::boot_resources::discard_static_plan();
    }
}

#[cfg(target_arch = "aarch64")]
#[unsafe(export_name = "efi_pe_entry")]
#[unsafe(link_section = ".text")]
pub unsafe extern "efiapi" fn efi_pe_entry(
    image_handle: Handle,
    system_table: *const ::core::ffi::c_void,
) -> Status {
    unsafe {
        if !crate::arch::relocate::try_apply() {
            return Status::LOAD_ERROR;
        }
        efi_pe_entry_main(image_handle, system_table)
    }
}

#[cfg(target_arch = "loongarch64")]
#[unsafe(naked)]
#[unsafe(export_name = "efi_pe_entry")]
#[unsafe(link_section = ".text")]
pub unsafe extern "efiapi" fn efi_pe_entry(
    _image_handle: Handle,
    _system_table: *const ::core::ffi::c_void,
) -> Status {
    naked_asm!(
        "
        move        $t0, $sp
        bstrins.d   $sp, $zero, 3, 0
        addi.d      $sp, $sp, -32
        st.d        $ra, $sp, 0
        st.d        $a0, $sp, 8
        st.d        $a1, $sp, 16
        st.d        $t0, $sp, 24

        la.pcrel    $t0, {bss_start}
        la.pcrel    $t1, {bss_stop}
    1:
        bgeu        $t0, $t1, 2f
        st.d        $zero, $t0, 0
        addi.d      $t0, $t0, 8
        b           1b
    2:
        dbar        0

        ld.d        $a0, $sp, 8
        ld.d        $a1, $sp, 16
        bl          {relocate}
        bnez        $a0, 3f
        li.d        $a0, {load_error}
        b           4f
    3:
        ld.d        $a0, $sp, 8
        ld.d        $a1, $sp, 16
        bl          {entry}

    4:
        ld.d        $t0, $sp, 24
        ld.d        $ra, $sp, 0
        move        $sp, $t0
        ret
        ",
        bss_start = sym __bss_start,
        bss_stop = sym __bss_stop,
        relocate = sym relocate,
        load_error = const Status::LOAD_ERROR.0,
        entry = sym efi_pe_entry_main,
    )
}

pub(crate) fn exit_boot_services() {
    if platform::inherited_handoff() {
        return;
    }
    println!("Exiting UEFI boot services...");
    // SAFETY: the boot CPU exclusively owns the memory-map buffers.
    let (mem_map, ()) = unsafe { exit_boot_services_no_alloc(|_| Ok(())) }
        .unwrap_or_else(|status| reset_on_exit_boot_services_failure(status));
    assert!(staged_initramfs_is_backed(mem_map.entries().copied()));
    // Inspect firmware configuration tables before SetVirtualAddressMap
    // rewrites their pointers to aliases installed by the kernel.
    memmap::setup_memory_map(mem_map.entries());
    finish_boot_services_exit(mem_map);
    platform::reserve_boot_memory();
    for descriptor in boot_data_reservations() {
        crate::mem::add_memory_descriptor(descriptor).expect("failed to reserve EFI boot data");
    }
    crate::boot_payload::publish_staged_uefi();
}

fn finish_boot_services_exit(mem_map: ExitBootMemoryMap) {
    // The firmware map has already been copied to separate static storage.
    // Reuse the original buffer for an in-place compacted virtual map, retaining
    // the firmware's descriptor stride, version, and trailing descriptor bytes.
    let map_ptr = unsafe { addr_of_mut!(EXIT_BOOT_MEMORY_MAP_BUFFER.0) }.cast::<u8>();
    let virtual_map_size = unsafe {
        build_virtual_address_map(
            mem_map.entries,
            map_ptr.cast_const(),
            map_ptr,
            mem_map.desc_size,
        )
    };

    unsafe {
        if let Some(st) = uefi::table::system_table_raw() {
            let virtual_system_table = platform::runtime_system_table(st.as_ptr());
            let runtime_services =
                st.as_ref().runtime_services.as_ref().unwrap_or_else(|| {
                    reset_on_exit_boot_services_failure(Status::INVALID_PARAMETER)
                });
            let status = (runtime_services.set_virtual_address_map)(
                virtual_map_size,
                mem_map.desc_size,
                mem_map.desc_version,
                map_ptr.cast::<MemoryDescriptor>(),
            );
            if status != Status::SUCCESS {
                reset_on_exit_boot_services_failure(status);
            }
            table::set_system_table(virtual_system_table);
        }
    }

    #[cfg(target_arch = "riscv64")]
    platform::retire_boot_page_tables();
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn exit_riscv_boot_services(
    fdt: &fdt_raw::Fdt<'_>,
    kernel: core::ops::Range<usize>,
    dtb_bytes: usize,
    hart_id: usize,
) -> core::result::Result<crate::mem::PreparedBootMemory, Status> {
    use core::alloc::Layout;

    let dtb_layout = Layout::from_size_align(dtb_bytes, 8).map_err(|_| Status::COMPROMISED_DATA)?;
    let cpu_layout =
        crate::smp::early_cpu_area_layout(hart_id).map_err(|_| Status::COMPROMISED_DATA)?;
    let mut map = crate::mem::MemoryMap::new();
    println!("Exiting UEFI boot services...");
    // SAFETY: the boot CPU owns the memory-map buffers. The callback only
    // reads firmware-owned data and prepares the kernel's private map.
    let (mem_map, prepared) = unsafe {
        exit_boot_services_no_alloc(|raw| {
            if !staged_initramfs_is_backed(raw.entries()) {
                return Err(Status::COMPROMISED_DATA);
            }
            memmap::prepare_fdt_memory_map(
                &mut map,
                fdt,
                kernel.clone(),
                raw,
                dtb_layout,
                cpu_layout,
            )
        })
    }?;
    finish_boot_services_exit(mem_map);
    crate::mem::replace_boot_map(map);
    if let Some(archive) = prepared.archive {
        crate::boot_payload::publish(archive.start, archive.end, archive.reclaimable);
    } else {
        crate::boot_payload::publish_staged_uefi();
    }
    Ok(prepared)
}

fn staged_initramfs_is_backed(mut entries: impl Iterator<Item = MemoryDescriptor>) -> bool {
    let Some((start, end)) = crate::boot_payload::staged_uefi() else {
        return true;
    };
    let Some(allocation_end) = end.checked_next_multiple_of(crate::consts::PAGE_SIZE) else {
        return false;
    };
    entries.any(|entry| {
        entry.ty == MemoryType::LOADER_DATA
            && entry.phys_start <= start as u64
            && entry
                .page_count
                .checked_mul(4096)
                .and_then(|size| entry.phys_start.checked_add(size))
                .is_some_and(|limit| allocation_end as u64 <= limit)
    })
}

fn boot_data_reservations() -> impl Iterator<Item = crate::mem::MemoryDescriptor> {
    use crate::mem::{MemoryDescriptor, MemoryType};

    #[cfg(feature = "boot-resources")]
    let resources = crate::boot_resources::snapshots()
        .into_iter()
        .filter(|resource| resource.id != 0)
        .map(|resource| {
            MemoryDescriptor::new_aligned(
                resource.range.start as usize,
                resource.range.len as usize,
                MemoryType::BootData,
                4096,
            )
        });
    #[cfg(not(feature = "boot-resources"))]
    let resources = core::iter::empty();

    resources.chain(crate::boot_payload::staged_uefi().map(|(start, end)| {
        assert!(end.checked_add(crate::consts::PAGE_SIZE - 1).is_some());
        MemoryDescriptor::new_aligned(
            start,
            end - start,
            MemoryType::BootData,
            crate::consts::PAGE_SIZE,
        )
    }))
}

fn load_firmware_entry_payload(image_handle: Handle) {
    setup_console();
    println!("UEFI application started.");
    load_efi_cmdline(image_handle);
    load_efi_payload();
}

pub(crate) fn init_boot_memory_map() {
    platform::init_boot_memory_map();
}

fn load_efi_payload() {
    let archive_staged = load_linux_initrd();
    if crate::cmdline::has_handoff_cmdline() && archive_staged {
        return;
    }

    let Ok(mut fs) = boot::get_image_file_system(boot::image_handle()) else {
        return;
    };
    let mut volume = fs.open_volume().expect("failed to open UEFI boot volume");
    if !crate::cmdline::has_handoff_cmdline()
        && let Ok(file) = volume.open(
            uefi::cstr16!("\\EFI\\BOOT\\cmdline.txt"),
            FileMode::Read,
            FileAttribute::empty(),
        )
    {
        let mut file = regular_file(file);
        let size = file_size(&mut file);
        assert!(size <= MAX_CMDLINE, "host command line is too long");
        let mut bytes = [0u8; MAX_CMDLINE];
        read_exact_file(&mut file, &mut bytes[..size]);
        let command = core::str::from_utf8(&bytes[..size]).expect("invalid host command line");
        assert!(!command.contains('\0'), "host command line contains NUL");
        crate::cmdline::set_cmdline(command.trim_end_matches(['\r', '\n']));
    }
    if !archive_staged
        && let Ok(file) = volume.open(
            uefi::cstr16!("\\EFI\\BOOT\\initramfs.cpio"),
            FileMode::Read,
            FileAttribute::empty(),
        )
    {
        let mut file = regular_file(file);
        let size = file_size(&mut file);
        assert!(
            size > 0 && size <= MAX_UEFI_INITRAMFS_BYTES,
            "invalid UEFI host archive size"
        );
        let pages = size.div_ceil(crate::consts::PAGE_SIZE);
        let image = boot::open_protocol_exclusive::<LoadedImage>(boot::image_handle())
            .expect("failed to inspect the loaded UEFI image");
        let (image_base, _) = image.info();
        let max_address = (image_base as usize)
            .checked_sub(1)
            .expect("loaded UEFI image has no lower address for host archive");
        let address = boot::allocate_pages(
            AllocateType::MaxAddress(max_address as _),
            MemoryType::LOADER_DATA,
            pages,
        )
        .expect("failed to allocate UEFI host archive below the loaded image");
        // SAFETY: allocate_pages returned at least `size` writable bytes.
        let bytes = unsafe { core::slice::from_raw_parts_mut(address.as_ptr(), size) };
        read_exact_file(&mut file, bytes);
        let start = address.as_ptr() as usize;
        crate::boot_payload::stage_uefi(
            start,
            start
                .checked_add(size)
                .expect("host archive range overflows"),
        );
        println!("UEFI host initramfs loaded: {size} bytes");
    }
}

fn load_linux_initrd() -> bool {
    let mut storage = [MaybeUninit::uninit(); LINUX_INITRD_DEVICE_PATH_SIZE];
    let mut device_path = build::DevicePathBuilder::with_buf(&mut storage)
        .push(&build::media::Vendor {
            vendor_guid: LINUX_EFI_INITRD_MEDIA_GUID,
            vendor_defined_data: &[],
        })
        .expect("failed to build Linux initrd device path")
        .finalize()
        .expect("failed to finalize Linux initrd device path");
    let handle = match boot::locate_device_path::<LoadFile2>(&mut device_path) {
        Ok(handle) => handle,
        Err(error) if error.status() == Status::NOT_FOUND => return false,
        Err(error) => panic!("failed to locate Linux initrd provider: {error:?}"),
    };
    let mut provider = boot::open_protocol_exclusive::<LoadFile2>(handle)
        .expect("failed to open Linux initrd provider");
    // SAFETY: `open_protocol_exclusive` keeps this firmware protocol instance
    // valid and uniquely borrowed for the lifetime of `provider`. `LoadFile2`
    // is a `#[repr(transparent)]` wrapper around the raw UEFI protocol, and
    // the size and alignment assertions above pin that representation at
    // compile time.
    let raw_provider = unsafe {
        &mut *((&mut *provider) as *mut LoadFile2
            as *mut uefi_raw::protocol::media::LoadFile2Protocol)
    };
    let mut size = 0usize;
    // SAFETY: the firmware-installed callback belongs to `raw_provider`; the
    // finalized device path and `size` pointer are valid for this call, and a
    // null buffer with `BootPolicy = FALSE` is the UEFI LoadFile2 size-query
    // contract. The firmware does not retain any of these pointers.
    let status = unsafe {
        (raw_provider.load_file)(
            raw_provider,
            device_path.as_ffi_ptr().cast(),
            uefi_raw::Boolean::FALSE,
            &mut size,
            core::ptr::null_mut(),
        )
    };
    assert_eq!(
        status,
        uefi::Status::BUFFER_TOO_SMALL,
        "Linux initrd provider did not report its size"
    );
    assert!(
        size > 0 && size <= MAX_UEFI_INITRAMFS_BYTES,
        "invalid Linux initrd size: {size}"
    );
    let pages = size.div_ceil(crate::consts::PAGE_SIZE);
    let address = boot::allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, pages)
        .expect("failed to allocate Linux initrd");
    let mut loaded = size;
    // SAFETY: this is the same exclusively opened provider and finalized
    // device path as the size query. `address` owns `pages` writable
    // `LOADER_DATA` pages, and the exact size reported by the query is passed
    // as the buffer capacity, so the firmware callback may write only within
    // that allocation through `address` and `loaded`.
    let status = unsafe {
        (raw_provider.load_file)(
            raw_provider,
            device_path.as_ffi_ptr().cast(),
            uefi_raw::Boolean::FALSE,
            &mut loaded,
            address.as_ptr().cast(),
        )
    };
    if status != Status::SUCCESS || loaded != size {
        // SAFETY: the allocation has not been handed to the kernel.
        unsafe { boot::free_pages(address, pages) }
            .expect("failed to free invalid Linux initrd allocation");
        panic!("Linux initrd provider read failed: {status:?}, bytes={loaded}");
    }
    let start = address.as_ptr() as usize;
    let end = start
        .checked_add(size)
        .expect("Linux initrd range overflows");
    crate::boot_payload::stage_uefi(start, end);
    println!("Linux EFI initrd loaded: {size} bytes");
    true
}

fn load_efi_cmdline(image_handle: Handle) {
    let image = boot::open_protocol_exclusive::<LoadedImage>(image_handle)
        .expect("failed to open LoadedImage for EFI command line");
    let options = match image.load_options_as_cstr16() {
        Ok(options) => options,
        Err(uefi::proto::loaded_image::LoadOptionsError::NotSet) => return,
        Err(error) => panic!("invalid EFI command line: {error:?}"),
    };
    let mut bytes = [0_u8; MAX_CMDLINE];
    match decode_efi_cmdline(options.to_u16_slice(), &mut bytes) {
        Ok("") => {}
        Ok(cmdline) => crate::cmdline::set_cmdline(cmdline),
        Err(error) => panic!("unsupported EFI command line: {error:?}"),
    }
}

fn regular_file(file: uefi::proto::media::file::FileHandle) -> RegularFile {
    match file.into_type().expect("failed to inspect UEFI boot file") {
        FileType::Regular(file) => file,
        FileType::Dir(_) => panic!("UEFI boot payload path is a directory"),
    }
}

fn file_size(file: &mut RegularFile) -> usize {
    #[repr(align(8))]
    struct InfoBuffer([u8; 512]);
    let mut buffer = InfoBuffer([0; 512]);
    let size = file
        .get_info::<FileInfo>(&mut buffer.0)
        .expect("failed to read UEFI boot file info")
        .file_size();
    usize::try_from(size).expect("UEFI boot file size overflows")
}

fn read_exact_file(file: &mut RegularFile, bytes: &mut [u8]) {
    let mut done = 0;
    while done < bytes.len() {
        let read = file
            .read(&mut bytes[done..])
            .expect("failed to read UEFI boot file");
        assert!(read != 0, "truncated UEFI boot file");
        done += read;
    }
}

pub(crate) fn boot_entropy() -> Option<[u8; 32]> {
    // An inherited LoongArch target is entered after the first loader has
    // exited boot services. Firmware protocol calls (including RNG discovery)
    // are no longer valid on that path; the target may still use a trusted FDT
    // seed or explicitly report that no firmware entropy is available.
    if platform::inherited_handoff() || !is_uefi_available() {
        return None;
    }

    let handle = boot::get_handle_for_protocol::<Rng>().ok()?;
    let mut rng = boot::open_protocol_exclusive::<Rng>(handle).ok()?;
    let mut seed = [0; 32];
    rng.get_rng(None, &mut seed).ok()?;
    Some(seed)
}

struct ExitBootMemoryMap {
    entries: &'static [MemoryDescriptor],
    desc_size: usize,
    desc_version: u32,
}

impl ExitBootMemoryMap {
    fn entries(&self) -> core::slice::Iter<'static, MemoryDescriptor> {
        self.entries.iter()
    }
}

/// # Safety
/// The boot CPU exclusively owns the map buffers and may call GetMemoryMap and
/// ExitBootServices. The validator may read the candidate map, but may not
/// call firmware or otherwise invalidate the map key.
unsafe fn exit_boot_services_no_alloc<T>(
    mut validate: impl FnMut(&RawBootMemoryMap<'_>) -> core::result::Result<T, Status>,
) -> core::result::Result<(ExitBootMemoryMap, T), Status> {
    let system_table = uefi::table::system_table_raw().ok_or(Status::INVALID_PARAMETER)?;
    let boot_services = unsafe {
        system_table
            .as_ref()
            .boot_services
            .as_ref()
            .ok_or(Status::INVALID_PARAMETER)?
    };
    let image_handle = saved_image_handle().ok_or(Status::INVALID_PARAMETER)?;

    for attempt in 0..EXIT_BOOT_MEMORY_MAP_RETRIES {
        // SAFETY: the boot CPU is the sole writer and a previous attempt
        // cannot retain its borrow of this fixed buffer.
        let buffer = unsafe { &mut *addr_of_mut!(EXIT_BOOT_MEMORY_MAP_BUFFER) };
        // SAFETY: the live EFI table contains the GetMemoryMap service.
        let map = unsafe { read_exit_boot_memory_map(boot_services.get_memory_map, buffer) };
        let map = map.and_then(|map| validate(&map).map(|prepared| (map, prepared)));
        let (map, prepared) = match map {
            Ok(map) => map,
            Err(error) if attempt == 0 => return Err(error),
            // Firmware may have partially stopped services on the first call.
            // Do not return to the loader or try to free its boot payload.
            Err(error) => reset_on_exit_boot_services_failure(error),
        };

        let status = unsafe { (boot_services.exit_boot_services)(image_handle.as_ptr(), map.key) };
        if status == Status::SUCCESS {
            UEFI_SERVICE_EXIT.store(true, core::sync::atomic::Ordering::Relaxed);
            let entries = unsafe { copy_exit_boot_memory_map(map.entries()) };
            return Ok((
                ExitBootMemoryMap {
                    entries,
                    desc_size: map.desc_size,
                    desc_version: map.desc_version,
                },
                prepared,
            ));
        }
        if status != Status::INVALID_PARAMETER {
            reset_on_exit_boot_services_failure(status);
        }
    }

    reset_on_exit_boot_services_failure(Status::INVALID_PARAMETER);
}

unsafe fn build_virtual_address_map(
    entries: &[MemoryDescriptor],
    source: *const u8,
    output: *mut u8,
    descriptor_size: usize,
) -> usize {
    assert!(
        descriptor_size >= size_of::<MemoryDescriptor>(),
        "UEFI memory descriptor size is too small"
    );
    assert!(
        entries
            .len()
            .checked_mul(descriptor_size)
            .is_some_and(|size| size <= EXIT_BOOT_MEMORY_MAP_BUFFER_SIZE),
        "UEFI virtual address map exceeds the exit-map buffer"
    );
    let mut count = 0;
    for (index, entry) in entries.iter().enumerate() {
        let virt_start = match entry.ty {
            MemoryType::RUNTIME_SERVICES_CODE | MemoryType::RUNTIME_SERVICES_DATA => {
                __va(entry.phys_start as _) as usize as u64
            }
            MemoryType::MMIO => __io(entry.phys_start as _) as usize as u64,
            _ => continue,
        };
        let source_entry = unsafe { source.add(index * descriptor_size) };
        let output_entry = unsafe { output.add(count * descriptor_size) };
        // SAFETY: `source_entry` covers one firmware descriptor and
        // `output_entry` stays within the same original map buffer. `copy`
        // permits the in-place, forward compaction used by the caller.
        unsafe { core::ptr::copy(source_entry, output_entry, descriptor_size) };
        unsafe {
            addr_of_mut!((*output_entry.cast::<MemoryDescriptor>()).virt_start)
                .write_unaligned(virt_start as _);
        }
        count += 1;
    }

    count * descriptor_size
}

unsafe fn copy_exit_boot_memory_map(
    entries: impl ExactSizeIterator<Item = MemoryDescriptor>,
) -> &'static [MemoryDescriptor] {
    let entry_count = entries.len();
    assert!(entry_count <= EXIT_BOOT_MEMORY_MAP_DESCRIPTOR_CAPACITY);
    let dst =
        addr_of_mut!(EXIT_BOOT_MEMORY_MAP_DESCRIPTORS).cast::<MaybeUninit<MemoryDescriptor>>();
    for (index, entry) in entries.enumerate() {
        // SAFETY: entry_count fits the destination, which this call exclusively
        // initializes after ExitBootServices succeeds.
        unsafe { dst.add(index).write(MaybeUninit::new(entry)) };
    }

    // SAFETY: the initialized copy is retained for the lifetime of the boot.
    unsafe { core::slice::from_raw_parts(dst.cast::<MemoryDescriptor>(), entry_count) }
}

fn save_image_handle(image_handle: Handle) {
    EFI_IMAGE_HANDLE.store(image_handle.as_ptr() as usize, Ordering::Relaxed);
}

fn saved_image_handle() -> Option<Handle> {
    let raw = EFI_IMAGE_HANDLE.load(Ordering::Relaxed);
    if raw == EFI_IMAGE_HANDLE_UNSET {
        return None;
    }

    unsafe { Handle::from_ptr(raw as *mut c_void) }
}

fn reset_on_exit_boot_services_failure(status: Status) -> ! {
    unsafe {
        if let Some(system_table) = uefi::table::system_table_raw()
            && let Some(runtime_services) = system_table.as_ref().runtime_services.as_ref()
        {
            (runtime_services.reset_system)(runtime::ResetType::COLD, status, 0, null());
        }
    }

    loop {
        core::hint::spin_loop();
    }
}

pub(crate) fn setup_console() {
    unsafe { crate::console::set_out(&UefiPrinter) };
}

#[allow(dead_code)]
fn efi_main() -> Result {
    find_fdt();
    find_acpi_rsdp();

    println!("Page size: {:#x} bytes", crate::mem::page_size());

    let h = boot::get_handle_for_protocol::<LoadedImage>()?;

    let img = boot::open_protocol_exclusive::<LoadedImage>(h)?;

    match img.load_options_as_cstr16() {
        Ok(cmdline) => {
            println!("Kernel command line: {}", cmdline);
        }
        Err(e) => {
            println!("Failed to get load options as CStr16: {:?}", e);
        }
    }

    Ok(())
}

static UEFI_SERVICE_EXIT: AtomicBool = AtomicBool::new(false);

struct UefiPrinter;
impl crate::console::Con for UefiPrinter {
    fn write_str(&self, s: &str) {
        if UEFI_SERVICE_EXIT.load(core::sync::atomic::Ordering::Relaxed) {
            return;
        }
        uefi::system::with_stdout(|stdout| {
            let _ = stdout.write_str(s);
        });
    }
}

fn find_fdt() {
    with_config_table(|config_table| {
        if let Some(addr) = find_fdt_address(config_table) {
            let fdt_addr =
                <crate::arch::Arch as crate::ArchTrait>::canonicalize_paddr(addr as usize);
            if crate::fdt::set_fdt_addr_phys_if_valid(fdt_addr) {
                println!("Found FDT at address: {:p}", addr);
            } else {
                println!("Ignoring invalid FDT at address: {:p}", addr);
            }
        } else {
            println!("No FDT found in UEFI config tables.");
        }
    })
}

fn find_fdt_address(config_table: &[ConfigTableEntry]) -> Option<*const c_void> {
    config_table
        .iter()
        .find(|entry| entry.guid == FDT_TABLE_GUID)
        .map(|entry| entry.address)
}

fn find_acpi_rsdp() {
    with_config_table(|config_table| {
        let mut version = 0;
        let mut addr = null();

        for entry in config_table {
            if entry.guid == ConfigTableEntry::ACPI2_GUID {
                // ACPI 2.0 RSDP (推荐)
                println!("Found ACPI 2.0 RSDP at address: {:p}", entry.address);
                version = 2;
                addr = entry.address;
                break;
            }

            if entry.guid == ConfigTableEntry::ACPI_GUID {
                // ACPI 1.0 RSDP (备选)
                println!("Found ACPI 1.0 RSDP at address: {:p}", entry.address);
                if version == 0 {
                    version = 1;
                    addr = entry.address;
                }
            }
        }

        if !addr.is_null() {
            println!("Using ACPI {} RSDP at address: {:p}", version, addr);
            let rsdp = <crate::arch::Arch as crate::ArchTrait>::canonicalize_paddr(addr as usize)
                as *const c_void;
            set_rsdp(rsdp);
        } else {
            println!("No ACPI RSDP found in UEFI config tables.");
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_fdt_table_address_by_uefi_guid() {
        let fdt_addr = 0x1234_0000usize as *const c_void;
        let acpi_addr = 0x5678_0000usize as *const c_void;
        let tables = [
            ConfigTableEntry {
                guid: ConfigTableEntry::ACPI2_GUID,
                address: acpi_addr,
            },
            ConfigTableEntry {
                guid: FDT_TABLE_GUID,
                address: fdt_addr,
            },
        ];

        assert_eq!(find_fdt_address(&tables), Some(fdt_addr));
    }

    #[test]
    fn boot_entropy_is_unavailable_without_uefi_system_table() {
        assert_eq!(boot_entropy(), None);
    }

    #[test]
    fn virtual_address_map_accepts_more_than_32_filtered_descriptors() {
        const DESCRIPTOR_SIZE: usize = size_of::<MemoryDescriptor>() + 8;
        let mut entries = [MemoryDescriptor::default(); 45];
        for (index, entry) in entries.iter_mut().enumerate() {
            entry.phys_start = 0x1000 * (index as u64 + 1);
            entry.page_count = index as u64 + 1;
            entry.ty = match index {
                3 => MemoryType::LOADER_DATA,
                11 => MemoryType::CONVENTIONAL,
                23 => MemoryType::MMIO_PORT_SPACE,
                31 => MemoryType::ACPI_RECLAIM,
                39 => MemoryType::RESERVED,
                0..=15 => MemoryType::RUNTIME_SERVICES_CODE,
                16..=31 => MemoryType::RUNTIME_SERVICES_DATA,
                32..=38 => MemoryType::MMIO,
                40..=42 => MemoryType::RUNTIME_SERVICES_CODE,
                43..=44 => MemoryType::RUNTIME_SERVICES_DATA,
                _ => unreachable!(),
            };
        }
        let mut raw_map = [0u8; 45 * DESCRIPTOR_SIZE];
        let mut selected_indices = [0usize; 40];
        let mut selected_count = 0;
        let mut source_markers = [0u8; 45];
        for (index, entry) in entries.iter().enumerate() {
            let offset = index * DESCRIPTOR_SIZE;
            let destination =
                unsafe { raw_map.as_mut_ptr().add(offset).cast::<MemoryDescriptor>() };
            unsafe { destination.write_unaligned(*entry) };
            let marker = 0x40u8.wrapping_add(index as u8);
            raw_map[offset + DESCRIPTOR_SIZE - 1] = marker;
            source_markers[index] = marker;
            if matches!(
                entry.ty,
                MemoryType::RUNTIME_SERVICES_CODE
                    | MemoryType::RUNTIME_SERVICES_DATA
                    | MemoryType::MMIO
            ) {
                selected_indices[selected_count] = index;
                selected_count += 1;
            }
        }
        assert_eq!(selected_count, 40);

        let mapped_size = unsafe {
            let map = raw_map.as_mut_ptr();
            build_virtual_address_map(&entries, map, map, DESCRIPTOR_SIZE)
        };

        assert_eq!(mapped_size, 40 * DESCRIPTOR_SIZE);
        for (output_index, source_index) in selected_indices.into_iter().enumerate() {
            let source = &entries[source_index];
            let offset = output_index * DESCRIPTOR_SIZE;
            let mapped = unsafe {
                raw_map
                    .as_ptr()
                    .add(offset)
                    .cast::<MemoryDescriptor>()
                    .read_unaligned()
            };
            assert_eq!(mapped.ty, source.ty);
            assert_eq!(mapped.phys_start, source.phys_start);
            assert_eq!(mapped.page_count, source.page_count);
            let expected: u64 = match source.ty {
                MemoryType::MMIO => __io(source.phys_start as _) as usize as u64,
                _ => __va(source.phys_start as _) as usize as u64,
            };
            assert_eq!(mapped.virt_start, expected);
            assert_eq!(
                raw_map[offset + DESCRIPTOR_SIZE - 1],
                source_markers[source_index]
            );
        }
    }
}

pub fn is_uefi_available() -> bool {
    uefi::table::system_table_raw().is_some()
}
