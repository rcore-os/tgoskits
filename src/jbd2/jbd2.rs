use core::error;

use alloc::vec;
use log::error;
use log::info;

use crate::BLOCK_SIZE_U32;
use crate::BlockDevError;
use crate::BlockDevice;
use crate::Jbd2Dev;
use crate::disknode::Ext4Extent;
use crate::disknode::Ext4Inode;
use crate::endian::DiskFormat;
use crate::ext4::*;
use crate::BlockDevResult;
use crate::jbd2::jbdstruct::JOURNAL_BLOCK_COUNT;
use crate::jbd2::jbdstruct::JOURNAL_FILE_INODE;
use crate::BLOCK_SIZE;
use crate::jbd2::jbdstruct::journal_superblock_s;
use crate::loopfile::resolve_inode_block;
use crate::mkfile::build_file_block_mapping;
use crate::mkfile::read_file;

///dump jouranl inode
pub fn dump_journal_inode<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
) {
    let mut indo = fs.get_inode_by_num(block_dev, 8).expect("journal");
    let datablock = resolve_inode_block(fs, block_dev,
         &mut indo, 0).unwrap().unwrap();
    let journal_data = fs.datablock_cache.get_or_load(block_dev, datablock as u64).unwrap().data.clone();
    let sb=journal_superblock_s::from_disk_bytes(&journal_data);
    error!("Journal Superblock:{:?}",sb);
    error!("Jouranl Inode:{:?}",indo);
}

///jouranl目录创建 journal超级块写入
pub fn create_journal_entry<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
) -> BlockDevResult<()> {
    //分配新数据块放superblock
    let journal_inode_num = JOURNAL_FILE_INODE;
    let free_block =fs.alloc_blocks(block_dev, 4096).expect("No enough block can alloc out!");
    //journal inode 额外参数
    let mut jour_inode = fs.get_inode_by_num(block_dev, journal_inode_num as u32).unwrap();
   jour_inode.write_extend_header();
   build_file_block_mapping(fs,&mut jour_inode,&free_block,block_dev);
    error!("When create jouranl inode: iblock:{:?}",jour_inode.i_block);
    let inode_size :usize= BLOCK_SIZE * free_block.len();
    //初始化 然后写入 journal inode
    fs.modify_inode(block_dev, journal_inode_num as u32,|inode|{
        inode.i_mode = Ext4Inode::S_IFREG | 0o600;
        inode.i_links_count=1;
        inode.i_size_lo = inode_size as u32;
        inode.i_flags = Ext4Inode::EXT4_EXTENTS_FL;
        inode.i_blocks_lo = (inode_size/512) as u32;       
        inode.i_block =jour_inode.i_block;
    }).expect("Jouranl inode create faild!");

    let mut jbd2_sb = journal_superblock_s::default();

    jbd2_sb.s_maxlen = free_block.len() as u32;//修正块数
    jbd2_sb.s_start=0;//相对于superblock
    jbd2_sb.s_blocksize=BLOCK_SIZE_U32;
    jbd2_sb.s_sequence=1;

    fs.datablock_cache.modify_new(free_block[0], |data|{
       jbd2_sb.to_disk_bytes(data);
    });
    info!("Journal inode created!");
    Ok(())
}



