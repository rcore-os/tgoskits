/// 位图用于跟踪块和inode的分配状态

/// 块位图包装结构
#[derive(Debug)]
pub struct BlockBitmap<'a> {
    data: &'a [u8],             // 位图数据
    blocks_per_group: u32,      // 每个块组的块数
}

impl<'a> BlockBitmap<'a> {
    /// 创建块位图实例
    pub fn new(data: &'a [u8], blocks_per_group: u32) -> Self {
        Self {
            data,
            blocks_per_group,
        }
    }

    /// 检查指定块是否已分配
    pub fn is_allocated(&self, block_idx: u32) -> Option<bool> {
        if block_idx >= self.blocks_per_group {
            return None;
        }
        
        let byte_idx = (block_idx / 8) as usize;
        let bit_idx = (block_idx % 8) as u8;
        
        if byte_idx >= self.data.len() {
            return None;
        }
        
        Some((self.data[byte_idx] & (1 << bit_idx)) != 0)
    }

    /// 检查指定块是否空闲
    pub fn is_free(&self, block_idx: u32) -> Option<bool> {
        self.is_allocated(block_idx).map(|allocated| !allocated)
    }

    /// 查找第一个空闲块
    /// 返回块组内的块索引
    pub fn find_first_free(&self) -> Option<u32> {
        for (byte_idx, &byte) in self.data.iter().enumerate() {
            if byte != 0xFF {
                // 这个字节有空闲位
                for bit_idx in 0..8 {
                    if (byte & (1 << bit_idx)) == 0 {
                        let block_idx = (byte_idx * 8 + bit_idx) as u32;
                        if block_idx < self.blocks_per_group {
                            return Some(block_idx);
                        }
                    }
                }
            }
        }
        None
    }

    /// 查找连续的空闲块
    /// count: 需要的连续块数
    pub fn find_contiguous_free(&self, count: u32) -> Option<u32> {
        if count == 0 {
            return None;
        }

        let mut consecutive = 0u32;
        let mut start_idx = 0u32;

        for block_idx in 0..self.blocks_per_group {
            if self.is_free(block_idx) == Some(true) {
                if consecutive == 0 {
                    start_idx = block_idx;
                }
                consecutive += 1;
                if consecutive == count {
                    return Some(start_idx);
                }
            } else {
                consecutive = 0;
            }
        }

        None
    }

    /// 统计空闲块数
    pub fn count_free(&self) -> u32 {
        let mut count = 0u32;
        
        for block_idx in 0..self.blocks_per_group {
            if self.is_free(block_idx) == Some(true) {
                count += 1;
            }
        }
        
        count
    }

    /// 统计已分配块数
    pub fn count_allocated(&self) -> u32 {
        self.blocks_per_group - self.count_free()
    }
}

/// 可变块位图包装结构
/// 用于修改位图
pub struct BlockBitmapMut<'a> {
    data: &'a mut [u8],         // 可变位图数据
    blocks_per_group: u32,      // 每个块组的块数
}

impl<'a> BlockBitmapMut<'a> {
    /// 创建可变块位图实例
    pub fn new(data: &'a mut [u8], blocks_per_group: u32) -> Self {
        Self {
            data,
            blocks_per_group,
        }
    }

    /// 检查指定块是否已分配
    pub fn is_allocated(&self, block_idx: u32) -> Option<bool> {
        if block_idx >= self.blocks_per_group {
            return None;
        }
        
        let byte_idx = (block_idx / 8) as usize;
        let bit_idx = (block_idx % 8) as u8;
        
        if byte_idx >= self.data.len() {
            return None;
        }
        
        Some((self.data[byte_idx] & (1 << bit_idx)) != 0)
    }

    /// 分配块（设置位为1） 自动mark为脏页
    pub fn allocate(&mut self, block_idx: u32) -> Result<(), BitmapError> {
        if block_idx >= self.blocks_per_group {
            return Err(BitmapError::IndexOutOfRange);
        }
        
        let byte_idx = (block_idx / 8) as usize;
        let bit_idx = (block_idx % 8) as u8;
        
        if byte_idx >= self.data.len() {
            return Err(BitmapError::IndexOutOfRange);
        }

        if (self.data[byte_idx] & (1 << bit_idx)) != 0 {
            return Err(BitmapError::AlreadyAllocated);
        }
        
        self.data[byte_idx] |= 1 << bit_idx;

        
        Ok(())
    }

    /// 释放块（设置位为0）
    pub fn free(&mut self, block_idx: u32) -> Result<(), BitmapError> {
        if block_idx >= self.blocks_per_group {
            return Err(BitmapError::IndexOutOfRange);
        }
        
        let byte_idx = (block_idx / 8) as usize;
        let bit_idx = (block_idx % 8) as u8;
        
        if byte_idx >= self.data.len() {
            return Err(BitmapError::IndexOutOfRange);
        }

        if (self.data[byte_idx] & (1 << bit_idx)) == 0 {
            return Err(BitmapError::AlreadyFree);
        }
        
        self.data[byte_idx] &= !(1 << bit_idx);
        Ok(())
    }

    /// 批量分配连续块
    pub fn allocate_range(&mut self, start_idx: u32, count: u32) -> Result<(), BitmapError> {
        // 先检查所有块是否都可用
        for i in 0..count {
            if self.is_allocated(start_idx + i) == Some(true) {
                return Err(BitmapError::AlreadyAllocated);
            }
        }

        // 执行分配
        for i in 0..count {
            self.allocate(start_idx + i)?;
        }

        Ok(())
    }

    /// 批量释放连续块
    pub fn free_range(&mut self, start_idx: u32, count: u32) -> Result<(), BitmapError> {
        for i in 0..count {
            self.free(start_idx + i)?;
        }
        Ok(())
    }
}

/// Inode位图包装结构
/// 每个块组都有自己的inode位图，用于跟踪该块组内的inode分配
#[derive(Debug)]
pub struct InodeBitmap<'a> {
    data: &'a [u8],             // 位图数据
    inodes_per_group: u32,      // 每个块组的inode数
}

impl<'a> InodeBitmap<'a> {
    /// 创建inode位图实例
    pub fn new(data: &'a [u8], inodes_per_group: u32) -> Self {
        Self {
            data,
            inodes_per_group,
        }
    }

    /// 检查指定inode是否已分配
    /// inode_idx: 块组内的inode索引（从0开始）
    pub fn is_allocated(&self, inode_idx: u32) -> Option<bool> {
        if inode_idx >= self.inodes_per_group {
            return None;
        }
        
        let byte_idx = (inode_idx / 8) as usize;
        let bit_idx = (inode_idx % 8) as u8;
        
        if byte_idx >= self.data.len() {
            return None;
        }
        
        Some((self.data[byte_idx] & (1 << bit_idx)) != 0)
    }

    /// 检查指定inode是否空闲
    pub fn is_free(&self, inode_idx: u32) -> Option<bool> {
        self.is_allocated(inode_idx).map(|allocated| !allocated)
    }

    /// 查找第一个空闲inode
    pub fn find_first_free(&self) -> Option<u32> {
        for (byte_idx, &byte) in self.data.iter().enumerate() {
            if byte != 0xFF {
                for bit_idx in 0..8 {
                    if (byte & (1 << bit_idx)) == 0 {
                        let inode_idx = (byte_idx * 8 + bit_idx) as u32;
                        if inode_idx < self.inodes_per_group {
                            return Some(inode_idx);
                        }
                    }
                }
            }
        }
        None
    }

    /// 统计空闲inode数
    pub fn count_free(&self) -> u32 {
        let mut count = 0u32;
        
        for inode_idx in 0..self.inodes_per_group {
            if self.is_free(inode_idx) == Some(true) {
                count += 1;
            }
        }
        
        count
    }

    /// 统计已分配inode数
    pub fn count_allocated(&self) -> u32 {
        self.inodes_per_group - self.count_free()
    }
}

/// 可变Inode位图包装结构
pub struct InodeBitmapMut<'a> {
    data: &'a mut [u8],         // 可变位图数据
    inodes_per_group: u32,      // 每个块组的inode数
}

impl<'a> InodeBitmapMut<'a> {
    /// 创建可变inode位图实例
    pub fn new(data: &'a mut [u8], inodes_per_group: u32) -> Self {
        Self {
            data,
            inodes_per_group,
        }
    }

    /// 检查指定inode是否已分配
    pub fn is_allocated(&self, inode_idx: u32) -> Option<bool> {
        if inode_idx >= self.inodes_per_group {
            return None;
        }
        
        let byte_idx = (inode_idx / 8) as usize;
        let bit_idx = (inode_idx % 8) as u8;
        
        if byte_idx >= self.data.len() {
            return None;
        }
        
        Some((self.data[byte_idx] & (1 << bit_idx)) != 0)
    }

    /// 分配inode（设置位为1）
    pub fn allocate(&mut self, inode_idx: u32) -> Result<(), BitmapError> {
        if inode_idx >= self.inodes_per_group {
            return Err(BitmapError::IndexOutOfRange);
        }
        
        let byte_idx = (inode_idx / 8) as usize;
        let bit_idx = (inode_idx % 8) as u8;
        
        if byte_idx >= self.data.len() {
            return Err(BitmapError::IndexOutOfRange);
        }

        if (self.data[byte_idx] & (1 << bit_idx)) != 0 {
            return Err(BitmapError::AlreadyAllocated);
        }
        
        self.data[byte_idx] |= 1 << bit_idx;
        Ok(())
    }

    /// 释放inode（设置位为0）
    pub fn free(&mut self, inode_idx: u32) -> Result<(), BitmapError> {
        if inode_idx >= self.inodes_per_group {
            return Err(BitmapError::IndexOutOfRange);
        }
        
        let byte_idx = (inode_idx / 8) as usize;
        let bit_idx = (inode_idx % 8) as u8;
        
        if byte_idx >= self.data.len() {
            return Err(BitmapError::IndexOutOfRange);
        }

        if (self.data[byte_idx] & (1 << bit_idx)) == 0 {
            return Err(BitmapError::AlreadyFree);
        }
        
        self.data[byte_idx] &= !(1 << bit_idx);
        Ok(())
    }
}

/// 位图操作错误类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitmapError {
    /// 索引超出范围
    IndexOutOfRange,
    /// 已经被分配
    AlreadyAllocated,
    /// 已经是空闲状态
    AlreadyFree,
}

impl core::fmt::Display for BitmapError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            BitmapError::IndexOutOfRange => write!(f, "位图索引超出范围"),
            BitmapError::AlreadyAllocated => write!(f, "已经被分配"),
            BitmapError::AlreadyFree => write!(f, "已经是空闲状态"),
        }
    }
}

/// 位图辅助函数
pub mod bitmap_utils {
    /// 计算存储n个位需要的字节数
    pub fn bytes_for_bits(bits: u32) -> usize {
        ((bits + 7) / 8) as usize
    }

    /// 计算字节中设置的位数（popcount）
    pub fn count_set_bits(byte: u8) -> u32 {
        byte.count_ones()
    }

    /// 计算位图中设置的位数
    pub fn count_set_bits_in_bitmap(data: &[u8], max_bits: u32) -> u32 {
        let mut count = 0u32;
        let full_bytes = (max_bits / 8) as usize;
        let remaining_bits = (max_bits % 8) as u8;

        // 完整的字节
        for &byte in &data[..full_bytes.min(data.len())] {
            count += count_set_bits(byte);
        }

        // 处理剩余的位
        if full_bytes < data.len() && remaining_bits > 0 {
            let mask = (1u8 << remaining_bits) - 1;
            count += count_set_bits(data[full_bytes] & mask);
        }

        count
    }

    /// 设置位图中的位
    pub fn set_bit(data: &mut [u8], bit_idx: u32) -> bool {
        let byte_idx = (bit_idx / 8) as usize;
        let bit_pos = (bit_idx % 8) as u8;
        
        if byte_idx >= data.len() {
            return false;
        }
        
        data[byte_idx] |= 1 << bit_pos;
        true
    }

    /// 清除位图中的位
    pub fn clear_bit(data: &mut [u8], bit_idx: u32) -> bool {
        let byte_idx = (bit_idx / 8) as usize;
        let bit_pos = (bit_idx % 8) as u8;
        
        if byte_idx >= data.len() {
            return false;
        }
        
        data[byte_idx] &= !(1 << bit_pos);
        true
    }

    /// 测试位图中的位
    pub fn test_bit(data: &[u8], bit_idx: u32) -> Option<bool> {
        let byte_idx = (bit_idx / 8) as usize;
        let bit_pos = (bit_idx % 8) as u8;
        
        if byte_idx >= data.len() {
            return None;
        }
        
        Some((data[byte_idx] & (1 << bit_pos)) != 0)
    }

    /// 切换位图中的位
    pub fn toggle_bit(data: &mut [u8], bit_idx: u32) -> bool {
        let byte_idx = (bit_idx / 8) as usize;
        let bit_pos = (bit_idx % 8) as u8;
        
        if byte_idx >= data.len() {
            return false;
        }
        
        data[byte_idx] ^= 1 << bit_pos;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn test_block_bitmap_basic() {
        let mut data = vec![0u8; 128]; // 1024位
        data[0] = 0b10101010; // 奇数位已分配
        
        let bitmap = BlockBitmap::new(&data, 1024);
        
        assert_eq!(bitmap.is_allocated(0), Some(false));
        assert_eq!(bitmap.is_allocated(1), Some(true));
        assert_eq!(bitmap.is_allocated(2), Some(false));
        assert_eq!(bitmap.is_allocated(3), Some(true));
    }

    #[test]
    fn test_block_bitmap_find_free() {
        let mut data = vec![0xFFu8; 128];
        data[10] = 0b11111101; // 第1位空闲
        
        let bitmap = BlockBitmap::new(&data, 1024);
        
        assert_eq!(bitmap.find_first_free(), Some(10 * 8 + 1));
    }

    #[test]
    fn test_block_bitmap_mut_allocate() {
        let mut data = vec![0u8; 128];
        let mut bitmap = BlockBitmapMut::new(&mut data, 1024);
        
        assert!(bitmap.allocate(5).is_ok());
        assert_eq!(bitmap.is_allocated(5), Some(true));
        assert_eq!(bitmap.allocate(5), Err(BitmapError::AlreadyAllocated));
    }

    #[test]
    fn test_inode_bitmap_basic() {
        let mut data = vec![0u8; 32]; // 256个inode
        data[0] = 0xFF; // 前8个已分配
        
        let bitmap = InodeBitmap::new(&data, 256);
        
        assert_eq!(bitmap.find_first_free(), Some(8));
        assert_eq!(bitmap.count_allocated(), 8);
    }

    #[test]
    fn test_bitmap_utils() {
        assert_eq!(bitmap_utils::bytes_for_bits(1), 1);
        assert_eq!(bitmap_utils::bytes_for_bits(8), 1);
        assert_eq!(bitmap_utils::bytes_for_bits(9), 2);
        assert_eq!(bitmap_utils::count_set_bits(0b10101010), 4);
    }
}
