use crate::ext4_backend::jbd2::*;
use crate::ext4_backend::config::*;
use crate::ext4_backend::jbd2::jbdstruct::*;
use crate::ext4_backend::endian::*;
use crate::ext4_backend::superblock::*;
use crate::ext4_backend::ext4::*;
use log::{debug, error};
use alloc::vec;
use alloc::vec::*;

///UUID生成 需要4个u32的uuid
pub fn generate_uuid()->UUID{
    //uuid生成策略 将函数指针进行异或
    let mut orign_uuid = [1_u32;4];
    let target_seed = debugSuperAndDesc as u32;
    let mut last_idx:usize =0;
    //首次异或
    orign_uuid[0]^=target_seed;
    //进行迭代异或    
    for idx in 0..orign_uuid.len()*2 {
        let real_idx = idx % orign_uuid.len();
        orign_uuid[real_idx] ^= orign_uuid[last_idx];
        last_idx=real_idx;
    }

    UUID(orign_uuid)
}

///UUID生成 需要16个u8的uuid
pub fn generate_uuid_8()->[u8;16]{
    //uuid生成策略 将函数指针进行异或
    let mut orign_uuid = [1_u8;16];
    let target_seed = debugSuperAndDesc as u8;
    let mut last_idx:usize =0;
    //首次异或
    orign_uuid[0]^=target_seed;
    //进行迭代异或    
    for idx in 0..orign_uuid.len()*2 {
        let real_idx = idx % orign_uuid.len();
        orign_uuid[real_idx] ^= orign_uuid[last_idx];
        last_idx=real_idx;
    }

    orign_uuid
}


pub fn debugSuperAndDesc(superblock:&Ext4Superblock,fs:&Ext4FileSystem){
    debug!("Superblock info: {:?}", &superblock);
    debug!("Block group descriptors:");
    let desc = &fs.group_descs;
    for gid in desc {
        debug!("Group descriptor: {:?}", gid);
    }
}

///是否需要redundance backup
pub fn need_redundant_backup(gid:u32)->bool{
   if gid==0 || gid==1 {
       return true;
   }
   let tmp_number  =gid as usize;
   let count:Vec<usize> = vec![3,5,7];
   for gid in count {
      if is_numbers_power(tmp_number, gid){
        return true;
      }
   }
   false

}
///number是不是base的幂
pub fn is_numbers_power(number:usize,base:usize)->bool{
    let mut tmp_number = number;
    if tmp_number == 1{
        return true;
    }
    while tmp_number%base==0 {
        tmp_number/=base;
    }
    if tmp_number==1 {
        return true;
    }else {
        return false;
    }
}

///根据块组号 计算块组布局（仅在 mkfs 阶段使用）
/// - `gid` 当前块组号
/// - `sb`  超级块（用于检查是否启用 sparse_super）
/// - `blocks_per_group` 每组块数
/// - `inode_table_blocks` 每组 inode 表占用的块数
/// - `group0_block_bitmap`/`group0_inode_bitmap`/`group0_inode_table` 组0的固定布局
/// - `gdt_blocks` 主 GDT 占用的块数（用于计算备份 GDT 大小）
pub fn cloc_group_layout(
    gid: u32,
    sb: &Ext4Superblock,
    blocks_per_group: u32,
    inode_table_blocks: u32,
    group0_block_bitmap: u32,
    group0_inode_bitmap: u32,
    group0_inode_table: u32,
    gdt_blocks: u32,
) -> BlcokGroupLayout {
    if gid == 0 {
        return BlcokGroupLayout {
            group_start_block: 0,
            group_blcok_bitmap_startblocks: group0_block_bitmap as u64,
            group_inode_bitmap_startblocks: group0_inode_bitmap as u64,
            group_inode_table_startblocks: group0_inode_table as u64,
            metadata_blocks_in_group: (group0_inode_table + inode_table_blocks) as u32,
        };
    }

    // 普通块组从其起始块开始布置
    let group_start = gid * blocks_per_group;

    // 是否启用 sparse super
    let sparse_feature = sb.has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER);

    // 是否在该组放置超级块 / GDT 备份
    let has_backup = sparse_feature && need_redundant_backup(gid);

    let (block_bitmap, inode_bitmap, inode_table, meta_blocks) = if has_backup {

        let bb = group_start + 1 + gdt_blocks;
        let ib = bb + 1;
        let it = ib + 1;
        let meta = 1 + gdt_blocks + 1 + 1 + inode_table_blocks;
        (bb, ib, it, meta)
    } else {

        let bb = group_start;
        let ib = group_start + 1;
        let it = group_start + 2;
        let meta = 1 + 1 + inode_table_blocks;
        (bb, ib, it, meta)
    };

    BlcokGroupLayout {
        group_start_block: group_start as u64,
        group_blcok_bitmap_startblocks: block_bitmap as u64,
        group_inode_bitmap_startblocks: inode_bitmap as u64,
        group_inode_table_startblocks: inode_table as u64,
        metadata_blocks_in_group: meta_blocks,
    }
}