#!/usr/bin/env python3
"""Convert a static riscv64 ELF (PIC, no relocations) into a UEFI PE32+ image.

Minimal stand-in for EDK2's GenFw: builds DOS header, PE/COFF headers, one
section per ELF PT_LOAD segment, and fills SizeOfImage / SizeOfHeaders so
loaders that read the optional header (like ArceBoot) map the full image.

SPDX-License-Identifier: Apache-2.0

Why this exists: the riscv64gc-unknown-uefi rustc target was removed from
rustc (LLVM has no riscv64 COFF object support) and GNU ld's pei-riscv64
emulation segfaults, so the GNU toolchain builds an ELF and this script
emits the PE container, mirroring what EDK2's GenFw does for its builds.
"""
import struct
import sys

IMAGE_FILE_MACHINE_RISCV64 = 0x5064
IMAGE_SUBSYSTEM_EFI_APPLICATION = 10
IMAGE_SCN_MEM_READ = 0x40000000
IMAGE_SCN_MEM_WRITE = 0x80000000
IMAGE_SCN_MEM_EXECUTE = 0x20000000
IMAGE_SCN_CNT_CODE = 0x00000020
IMAGE_SCN_CNT_INITIALIZED_DATA = 0x00000040

FILE_ALIGN = 0x200
SECT_ALIGN = 0x1000


def align_up(v, a):
    return (v + a - 1) & ~(a - 1)


def main(elf_path, pe_path):
    elf = open(elf_path, "rb").read()

    # --- parse ELF header (64-bit little-endian) ---
    (e_entry,) = struct.unpack_from("<Q", elf, 24)
    (e_phoff,) = struct.unpack_from("<Q", elf, 32)
    (e_phentsize, e_phnum) = struct.unpack_from("<HH", elf, 54)
    loads = []
    for i in range(e_phnum):
        off = e_phoff + i * e_phentsize
        (p_type, p_flags, p_offset, p_vaddr, _p_paddr, p_filesz, p_memsz, _align) = (
            struct.unpack_from("<IIQQQQQQ", elf, off)
        )
        if p_type == 1:  # PT_LOAD
            loads.append((p_offset, p_vaddr, p_filesz, p_memsz, p_flags))

    if not loads:
        sys.exit("no PT_LOAD segments")

    image_end = max(vaddr + memsz for _, vaddr, _, memsz, _ in loads)
    entry_rva = e_entry  # image base is 0

    # --- headers ---
    dos_size = 0x40
    pe_off = 0x80
    opt_size = 0xF0  # PE32+ optional header
    sect_size = 0x28
    headers_size = align_up(pe_off + 4 + 20 + opt_size + sect_size * len(loads), FILE_ALIGN)
    size_of_image = align_up(image_end, SECT_ALIGN)

    out = bytearray()

    # DOS header + stub padding up to the PE signature
    out += b"MZ" + b"\x00" * (dos_size - 4)
    out[0x3C:0x40] = struct.pack("<I", pe_off)
    out += b"\x00" * (pe_off - len(out))

    # PE signature + COFF header
    out += b"PE\x00\x00"
    out += struct.pack("<HHIIIHH",
                       IMAGE_FILE_MACHINE_RISCV64,  # machine
                       len(loads),                  # number of sections
                       0,                            # timestamp
                       0,                            # ptr to symbol table
                       0,                            # number of symbols
                       opt_size,                     # size of optional header
                       0x0002)                       # characteristics: EXECUTABLE_IMAGE

    # Optional header (PE32+)
    opt = bytearray()
    opt += struct.pack("<HBB", 0x20B, 0, 0)                      # magic, linker ver
    opt += struct.pack("<I", 0)                                  # size of code
    opt += struct.pack("<I", 0)                                  # size of initialized data
    opt += struct.pack("<I", 0)                                  # size of uninitialized data
    opt += struct.pack("<I", entry_rva)                          # entry point RVA
    opt += struct.pack("<I", 0)                                  # base of code
    opt += struct.pack("<Q", 0)                                  # image base
    opt += struct.pack("<II", SECT_ALIGN, FILE_ALIGN)            # alignments
    opt += struct.pack("<HHHH", 0, 0, 0, 0)                      # OS + image versions
    opt += struct.pack("<HH", 2, 0)                              # subsystem version
    opt += struct.pack("<I", 0)                                  # win32 version
    opt += struct.pack("<I", size_of_image)                      # size of image
    opt += struct.pack("<I", headers_size)                       # size of headers
    opt += struct.pack("<I", 0)                                  # checksum
    opt += struct.pack("<HH", IMAGE_SUBSYSTEM_EFI_APPLICATION, 0)  # subsystem, dll flags
    opt += struct.pack("<QQ", 0, 0)                              # stack reserve/commit
    opt += struct.pack("<QQ", 0, 0)                              # heap reserve/commit
    opt += struct.pack("<II", 0, 16)                             # loader flags, # dirs
    opt += b"\x00" * (16 * 8)                                    # 16 empty data directories
    assert len(opt) == opt_size
    out += opt

    # Section table, then raw data: one section per PT_LOAD. The raw layout
    # is contiguous per section so `SizeOfRawData` matches the bytes a PE
    # loader copies for each `VirtualAddress`.
    raw_cursor = headers_size
    raw_blobs = []
    for idx, (offset, vaddr, filesz, memsz, flags) in enumerate(loads):
        executable = bool(flags & 1)
        writable = bool(flags & 2)
        characteristics = IMAGE_SCN_MEM_READ | IMAGE_SCN_CNT_INITIALIZED_DATA
        if executable:
            characteristics |= IMAGE_SCN_MEM_EXECUTE | IMAGE_SCN_CNT_CODE
        if writable:
            characteristics |= IMAGE_SCN_MEM_WRITE

        raw_size = align_up(filesz, FILE_ALIGN)
        name = b".text" if executable else b".data"
        out += name.ljust(8, b"\x00")
        out += struct.pack("<IIIIIIHHI",
                           memsz,          # VirtualSize
                           vaddr,          # VirtualAddress
                           raw_size,       # SizeOfRawData
                           raw_cursor,     # PointerToRawData
                           0, 0, 0, 0,     # relocs, linenums, counts
                           characteristics)
        blob = bytearray(elf[offset : offset + filesz])
        blob += b"\x00" * (raw_size - len(blob))
        raw_blobs.append(blob)
        raw_cursor += raw_size

    assert len(out) <= headers_size
    out += b"\x00" * (headers_size - len(out))
    for blob in raw_blobs:
        out += blob

    open(pe_path, "wb").write(out)
    print(f"{pe_path}: entry_rva=0x{entry_rva:x} size_of_image=0x{size_of_image:x} "
          f"headers=0x{headers_size:x} sections={len(loads)}")


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
