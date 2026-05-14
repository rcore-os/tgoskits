#![allow(dead_code)]

pub const LINUX_EFISTUB_MAJOR_VERSION: usize = 0x3;
pub const LINUX_EFISTUB_MINOR_VERSION: usize = 0x0;

pub const LINUX_PE_MAGIC: usize = 0x818223cd;

pub const IMAGE_DOS_SIGNATURE: usize = 0x5A4D;

pub const IMAGE_NT_SIGNATURE: usize = 0x00004550;

// IMAGE_FILE 标志位常量
pub const IMAGE_FILE_RELOCS_STRIPPED: u16 = 0x0001; // Relocation info stripped from file
pub const IMAGE_FILE_EXECUTABLE_IMAGE: u16 = 0x0002; // File is executable (i.e. no unresolved external references)
pub const IMAGE_FILE_LINE_NUMS_STRIPPED: u16 = 0x0004; // Line numbers stripped from file
pub const IMAGE_FILE_LOCAL_SYMS_STRIPPED: u16 = 0x0008; // Local symbols stripped from file
pub const IMAGE_FILE_AGGRESSIVE_WS_TRIM: u16 = 0x0010; // Aggressively trim working set
pub const IMAGE_FILE_LARGE_ADDRESS_AWARE: u16 = 0x0020; // App can handle >2gb addresses (image can be loaded at address above 2GB)
pub const IMAGE_FILE_16BIT_MACHINE: u16 = 0x0040; // 16 bit word machine
pub const IMAGE_FILE_BYTES_REVERSED_LO: u16 = 0x0080; // Bytes of machine word are reversed (should be set together with IMAGE_FILE_BYTES_REVERSED_HI)
pub const IMAGE_FILE_32BIT_MACHINE: u16 = 0x0100; // 32 bit word machine
pub const IMAGE_FILE_DEBUG_STRIPPED: u16 = 0x0200; // Debugging info stripped from file in .DBG file
pub const IMAGE_FILE_REMOVABLE_RUN_FROM_SWAP: u16 = 0x0400; // If Image is on removable media, copy and run from the swap file
pub const IMAGE_FILE_NET_RUN_FROM_SWAP: u16 = 0x0800; // If Image is on Net, copy and run from the swap file
pub const IMAGE_FILE_SYSTEM: u16 = 0x1000; // System kernel-mode file (can't be loaded in user-mode)
pub const IMAGE_FILE_DLL: u16 = 0x2000; // File is a DLL
pub const IMAGE_FILE_UP_SYSTEM_ONLY: u16 = 0x4000; // File should only be run on a UP (uniprocessor) machine
pub const IMAGE_FILE_BYTES_REVERSED_HI: u16 = 0x8000; // Bytes of machine word are reversed (should be set together with IMAGE_FILE_BYTES_REVERSED_LO)

/// Extensible Firmware Interface (EFI) application
pub const IMAGE_SUBSYSTEM_EFI_APPLICATION: usize = 10;

pub const IMAGE_FILE_MACHINE_AMD64: usize = 0x8664;
pub const IMAGE_FILE_MACHINE_LOONGARCH64: usize = 0x6264;
