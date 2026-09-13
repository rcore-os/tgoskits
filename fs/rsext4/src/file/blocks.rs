use super::*;

/// Builds block mappings and enables checksums for external extent nodes.
pub fn build_file_block_mapping_with_inode_num<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    inode: &mut Ext4Inode,
    inode_num: InodeNumber,
    data_blocks: &[AbsoluteBN],
    block_dev: &mut Jbd2Dev<B>,
) -> Ext4Result<()> {
    if data_blocks.is_empty() {
        inode.i_blocks_lo = 0;
        inode.l_i_blocks_high = 0;
        inode.i_block = [0; 15];
        return Ok(());
    }

    if fs.superblock.has_extents() {
        // Prefer extents and merge contiguous physical blocks into the same run.
        inode.i_flags |= Ext4Inode::EXT4_EXTENTS_FL;
        inode.i_block = [0; 15];
        inode.write_extend_header();

        let mut exts_vec: Vec<Ext4Extent> = Vec::new();

        let mut run_start_lbn: u32 = 0;
        let mut run_start_pblk = data_blocks[0].raw();
        let mut run_len: u32 = 1;

        for (idx, &pblk) in data_blocks.iter().enumerate().skip(1) {
            let lbn = idx as u32;
            let prev_lbn = lbn - 1;
            let prev_pblk = data_blocks[prev_lbn as usize].raw();
            let pblk = pblk.raw();

            let is_contiguous = pblk == prev_pblk.saturating_add(1);

            if is_contiguous {
                run_len = run_len.saturating_add(1);
            } else {
                // Finish the current physical run and emit one extent.
                let ext = Ext4Extent::new(run_start_lbn, run_start_pblk, run_len as u16);
                exts_vec.push(ext);

                run_start_lbn = lbn;
                run_start_pblk = pblk;
                run_len = 1;
            }
        }

        let ext = Ext4Extent::new(run_start_lbn, run_start_pblk, run_len as u16);
        exts_vec.push(ext);

        // Insert the computed extents through `ExtentTree` so the inode root
        // receives the same serialized structure as runtime writes.
        let mut tree = ExtentTree::with_filesystem(inode, fs, inode_num);
        for extend in exts_vec {
            tree.insert_extent(fs, extend, block_dev)?;
        }
    } else {
        if data_blocks.len() > 12 {
            return Err(Ext4Error::unsupported());
        }
        for (logical, physical) in data_blocks.iter().enumerate() {
            inode.i_block[logical] = physical.to_u32()?;
        }
    }
    Ok(())
}
