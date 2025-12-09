use core::error;

use log::error;
use log::info;

use crate::BLOCK_SIZE_U32;
use crate::BlockDevError;
use crate::BlockDevice;
use crate::disknode::Ext4Extent;
use crate::disknode::Ext4Inode;
use crate::endian::DiskFormat;
use crate::ext4::*;
use crate::BlockDev;
use crate::BlockDevResult;
use crate::jbd2::jbdstruct::JOURNAL_BLOCK_COUNT;
use crate::jbd2::jbdstruct::JOURNAL_FILE_INODE;
use crate::BLOCK_SIZE;
use crate::jbd2::jbdstruct::journal_superblock_s;
use crate::loopfile::resolve_inode_block;
use crate::mkd::build_single_block_dir_mapping;
use crate::mkfile::read_file;

///dump jouranl inode
pub fn dump_journal_inode<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut BlockDev<B>,
) -> BlockDevResult<()> {
    let journal_inode_num = JOURNAL_FILE_INODE;
   
    let inode_table_start = fs.group_descs.get(0).expect("Group 0 can;t get!")
    .inode_table();
    let (block_num, offset, _group_idx) = fs.inodetable_cahce.calc_inode_location(
    JOURNAL_FILE_INODE as u32,
    fs.superblock.s_inodes_per_group,
    inode_table_start,
    BLOCK_SIZE,
    );

    let mut indo=fs.inodetable_cahce.get_or_load(block_dev,8,block_num,offset).expect("journal").inode;
    let datablock = resolve_inode_block(fs, block_dev,
         &mut indo, 0).unwrap().unwrap();
    let mut journal_data = fs.datablock_cache.get_or_load(block_dev, datablock as u64).unwrap().data.clone();
    let sb=journal_superblock_s::from_disk_bytes(&journal_data);

    error!("Journal Superblock:{:?}",sb);
    error!("Jouranl Inode:{:?}",indo);
        

  
    Ok(())
}

///jouranl目录创建 journal超级块写入
pub fn create_journal_entry<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut BlockDev<B>,
) -> BlockDevResult<()> {
    //分配新数据块放superblock
    let journal_inode_num = JOURNAL_FILE_INODE;
    let group_idx = fs.find_group_with_free_blocks()
    .ok_or(BlockDevError::NoSpace)?;
    let free_block =fs.alloc_block(block_dev, group_idx).expect("No enough block can alloc out!");

    let inode_table_start = fs.group_descs.get(0).expect("Group 0 can;t get!")
    .inode_table();
    let (block_num, offset, _group_idx) = fs.inodetable_cahce.calc_inode_location(
    JOURNAL_FILE_INODE as u32,
    fs.superblock.s_inodes_per_group,
    inode_table_start,
    BLOCK_SIZE,
    );

    //journal inode 额外参数
    let iblocks =  build_single_block_dir_mapping(fs,free_block);
    let inode_size :u32= BLOCK_SIZE_U32;//16mb
    //初始化 然后写入 journal inode
    fs.inodetable_cahce.modify(block_dev, journal_inode_num, block_num, offset, |inode|{
        inode.i_mode = Ext4Inode::S_IFREG | 0o600;
        inode.i_links_count=1;
        inode.i_size_lo = inode_size;
        inode.i_flags = Ext4Inode::EXT4_EXTENTS_FL;
        inode.i_blocks_lo = inode_size/512;
        inode.i_block = iblocks.1;
       
    }).expect("Jouranl inode create faild!");

    let mut jbd2_sb = journal_superblock_s::default();
    fs.datablock_cache.modify_new(free_block, |data|{
       jbd2_sb.to_disk_bytes(data);
    });
    info!("Journal inode created!");
    
    Ok(())
}