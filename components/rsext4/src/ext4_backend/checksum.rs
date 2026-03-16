use crate::{
    BLOCK_SIZE,
    ext4_backend::{
        crc32c::crc32c::*, disknode::Ext4Inode, endian::DiskFormat, entries::Ext4DirEntryTail,
        jbd2::jbdstruct::JournalSuperBllockS, superblock::Ext4Superblock,
    },
};
extern crate alloc;

/// 计算 ext4 `metadata_csum` 使用的 CRC32C（32位）。
///
/// 与内核 ext4_chksum 链式调用对齐：
/// - `seed` 作为初始 CRC 状态（不是作为数据喂入）
/// - 按顺序追加 `parts` 中的所有字节片段
/// - 返回 raw CRC（不做 finalize）
pub fn ext4_metadata_csum32(seed: u32, parts: &[&[u8]]) -> u32 {
    let mut crc = seed;
    for p in parts {
        crc = crc32c_append(crc, p);
    }
    crc
}

/// 计算 superblock 的 `s_checksum`（`metadata_csum` 特性）。
///
/// 内核实现：ext4_superblock_csum(sb, es)
///   csum = ext4_chksum(sbi, ~0, (char *)es, offsetof(s_checksum));
/// 即：初始值 ~0，对前 1020 字节（不含 s_checksum 字段本身）计算 raw CRC32C。
/// 不使用 seed，不做 finalize。
pub fn ext4_superblock_csum32(sb: &Ext4Superblock) -> u32 {
    let mut sb_bytes = [0u8; Ext4Superblock::SUPERBLOCK_SIZE];
    sb.to_disk_bytes(&mut sb_bytes);
    // s_checksum 位于超级块末尾 4 字节，只对前 1020 字节计算
    let offset = Ext4Superblock::SUPERBLOCK_SIZE - 4;
    crc32c_append(crc32c_init(), &sb_bytes[..offset])
}

/// 根据 superblock 的特性位，决定是否更新 `s_checksum`。
///
/// 只有在开启 `EXT4_FEATURE_RO_COMPAT_METADATA_CSUM` 时才会更新，
/// 否则保持原值不变。
pub fn ext4_update_superblock_checksum(sb: &mut Ext4Superblock) {
    if ext4_superblock_has_metadata_csum(sb) {
        sb.s_checksum = ext4_superblock_csum32(sb);
    }
}

/// 计算块组描述符（GDT）的校验和并返回 16 位。
///
/// ext4 在某些场景下会把 CRC32C 的低 16 位存入 `bg_checksum`。
/// 此函数只提供计算入口，是否/何时启用仍需根据 feature 位与布局规则决定。
#[allow(dead_code)]
pub fn ext4_group_desc_csum16(sb: &Ext4Superblock, group_id: u32, desc_bytes: &[u8]) -> u16 {
    let seed = ext4_crc32c_seed_from_superblock(sb);
    let group_id_le = group_id.to_le_bytes();
    let csum = ext4_metadata_csum32(seed, &[&group_id_le, desc_bytes]);
    (csum & 0xFFFF) as u16
}

/// 计算 inode 的 CRC32C 校验和（32位），用于写回 inode 的 checksum 字段。
///
/// 实现要点：
/// 1) 把 inode 的 `l_i_checksum_lo` / `i_checksum_hi` 清 0。
/// 2) 对 inode 磁盘序字节流计算。
/// 3) 输入形式：`seed + uuid + inode_num + inode_bytes`。
#[allow(dead_code)]
pub fn ext4_inode_csum32(
    sb: &Ext4Superblock,
    inode_num: u32,
    generation: u32,
    inode: &Ext4Inode,
    inode_size: usize,
) -> u32 {
    let seed = ext4_crc32c_seed_from_superblock(sb);
    let inode_num_le = inode_num.to_le_bytes();
    let gen_le = generation.to_le_bytes();

    let mut inode_bytes = alloc::vec![0u8; inode_size];
    let mut inode_for_csum = *inode;
    // 校验字段自身必须当作 0
    inode_for_csum.l_i_checksum_lo = 0;
    inode_for_csum.i_checksum_hi = 0;
    inode_for_csum.to_disk_bytes(&mut inode_bytes[..]);

    ext4_metadata_csum32(seed, &[&inode_num_le, &gen_le, &inode_bytes])
}

/// 计算并写回 inode 的 checksum 字段。
///
/// ext4 inode 的 checksum 以两个 `u16` 形式存放：
/// - `l_i_checksum_lo` = checksum 低 16 位
/// - `i_checksum_hi`   = checksum 高 16 位
#[allow(dead_code)]
pub fn ext4_update_inode_checksum(
    sb: &Ext4Superblock,
    inode_num: u32,
    generation: u32,
    inode: &mut Ext4Inode,
    inode_size: usize,
) {
    let csum = ext4_inode_csum32(sb, inode_num, generation, inode, inode_size);
    // 拆分写回到两个 16 位字段
    inode.l_i_checksum_lo = (csum & 0xFFFF) as u16;
    inode.i_checksum_hi = ((csum >> 16) & 0xFFFF) as u16;
}

/// 对“任意一段元数据块内容”计算 CRC32C（32位）。
///
/// 这是一个通用入口：输入形式 `seed + data`。
/// 如果某个结构需要额外拼接（例如 uuid、inode 号、group id 等），
/// 应该写专用 helper，而不是只用这个函数。
#[allow(dead_code)]
pub fn ext4_metadata_block_csum32(sb: &Ext4Superblock, data: &[u8]) -> u32 {
    let seed = ext4_crc32c_seed_from_superblock(sb);
    ext4_metadata_csum32(seed, &[data])
}

/// 校验目录块的 checksum 是否正确（读路径使用）。
/// 返回 true 表示校验通过或未启用 metadata_csum。
pub fn verify_ext4_dirblock_checksum(
    sb: &Ext4Superblock,
    ino: u32,
    generation: u32,
    block_bytes: &[u8],
) -> bool {
    if !ext4_superblock_has_metadata_csum(sb) {
        return true;
    }
    if block_bytes.len() < BLOCK_SIZE || BLOCK_SIZE < 12 {
        return false;
    }
    let tail_ft = block_bytes[BLOCK_SIZE - 5]; // det_reserved_ft 位于 tail 偏移 7
    if tail_ft != 0xDE {
        return true; // 没有合法 tail，跳过校验
    }
    let stored = u32::from_le_bytes([
        block_bytes[BLOCK_SIZE - 4],
        block_bytes[BLOCK_SIZE - 3],
        block_bytes[BLOCK_SIZE - 2],
        block_bytes[BLOCK_SIZE - 1],
    ]);
    // 内核只校验 tail 之前的数据：size = (char*)t - (char*)bh->b_data = BLOCK_SIZE - 12
    let data_len = BLOCK_SIZE - Ext4DirEntryTail::TAIL_LEN as usize;
    let computed = ext4_dirblock_csum32(sb, ino, generation, &block_bytes[..data_len]);
    computed == stored
}

/// 更新目录块（dirblock）的 CRC32C（32位）便捷封装
pub fn update_ext4_dirblock_csum32(
    sb: &Ext4Superblock,
    parent_dir_ino: u32,
    generation: u32,
    block_bytes: &mut [u8],
) {
    let has_checksum = ext4_superblock_has_metadata_csum(sb);
    if has_checksum {
        // 内核只对 tail 之前的数据计算 checksum：size = BLOCK_SIZE - 12
        let data_len = BLOCK_SIZE - Ext4DirEntryTail::TAIL_LEN as usize;
        let csum = ext4_dirblock_csum32(sb, parent_dir_ino, generation, &block_bytes[..data_len]);
        let tail_checksum = &mut block_bytes[BLOCK_SIZE - 4..];
        tail_checksum.copy_from_slice(&csum.to_le_bytes());
    }
}

/// 计算目录块（dirblock）的 CRC32C（32位）。
///
/// 目录块的 checksum 与目录自身的 inode 号和 i_generation 绑定，
/// 避免跨目录搬运或 inode 复用后误通过校验。
/// 输入形式：`seed + uuid + ino + gen + block_bytes`。
#[allow(dead_code)]
pub fn ext4_dirblock_csum32(
    sb: &Ext4Superblock,
    ino: u32,
    generation: u32,
    block_bytes: &[u8],
) -> u32 {
    let seed = ext4_crc32c_seed_from_superblock(sb);
    let ino_le = ino.to_le_bytes();
    let gen_le = generation.to_le_bytes();
    ext4_metadata_csum32(seed, &[&ino_le, &gen_le, block_bytes])
}

/// 更新目录块尾部（`Ext4DirEntryTail`）中的 `det_checksum`。
///
/// 参数：
/// - `block_bytes`: 整个目录块（通常一个 block 大小）
/// - `tail_offset`: tail 在块内的起始偏移
///
/// 行为：
/// 1) 先把 tail 内 `det_checksum` 的 4 字节清 0。
/// 2) 对整个 `block_bytes` 计算目录块 checksum。
/// 3) 将结果写回 `det_checksum`（按小端）。
#[allow(dead_code)]
pub fn ext4_update_dirblock_tail_checksum(
    sb: &Ext4Superblock,
    ino: u32,
    generation: u32,
    block_bytes: &mut [u8],
    tail_offset: usize,
) {
    if tail_offset + 12 > block_bytes.len() {
        return;
    }

    // det_checksum 位于 tail 内偏移 8..12
    block_bytes[tail_offset + 8..tail_offset + 12].fill(0);
    // 内核只对 tail 之前的数据计算 checksum：size = tail_offset = BLOCK_SIZE - 12
    let csum = ext4_dirblock_csum32(sb, ino, generation, &block_bytes[..tail_offset]);
    block_bytes[tail_offset + 8..tail_offset + 12].copy_from_slice(&csum.to_le_bytes());
}

/// 计算 JBD2 journal superblock 的 checksum（32位）。
///
/// 注意：JBD2 的 checksum 不使用 ext4 的 seed；这里只做“把 checksum 字段置 0 后
/// 对整个 journal superblock 字节流做 CRC32C”。
#[allow(dead_code)]
pub fn jbd2_superblock_csum32(jsb: &JournalSuperBllockS) -> u32 {
    let mut bytes = [0u8; 1024];
    let mut jsb_for_csum = *jsb;
    // 校验字段自身必须当作 0
    jsb_for_csum.s_checksum = 0;
    jsb_for_csum.to_disk_bytes(&mut bytes);
    crc32c(&bytes)
}

/// 更新 JBD2 journal superblock 的 `s_checksum` 字段。
#[allow(dead_code)]
pub fn jbd2_update_superblock_checksum(jsb: &mut JournalSuperBllockS) {
    jsb.s_checksum = jbd2_superblock_csum32(jsb);
}

/// 计算 block bitmap 的 checksum（32位）。
///
/// 内核：csum = ext4_chksum(s_csum_seed, bitmap_data, clusters_per_group/8)
/// 只有 seed + bitmap 数据，不含 group_id。
#[allow(dead_code)]
pub fn ext4_block_bitmap_csum32(sb: &Ext4Superblock, bitmap_bytes: &[u8]) -> u32 {
    let seed = ext4_crc32c_seed_from_superblock(sb);
    let sz = (sb.s_clusters_per_group as usize) / 8;
    let sz = core::cmp::min(sz, bitmap_bytes.len());
    ext4_metadata_csum32(seed, &[&bitmap_bytes[..sz]])
}

/// 计算 inode bitmap 的 checksum（32位）。
///
/// 内核：csum = ext4_chksum(s_csum_seed, bitmap_data, inodes_per_group/8)
/// 只有 seed + bitmap 数据，不含 group_id。
#[allow(dead_code)]
pub fn ext4_inode_bitmap_csum32(sb: &Ext4Superblock, bitmap_bytes: &[u8]) -> u32 {
    let seed = ext4_crc32c_seed_from_superblock(sb);
    let sz = (sb.s_inodes_per_group as usize) / 8;
    let sz = core::cmp::min(sz, bitmap_bytes.len());
    ext4_metadata_csum32(seed, &[&bitmap_bytes[..sz]])
}
