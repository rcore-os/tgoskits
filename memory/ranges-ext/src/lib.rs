#![cfg_attr(target_os = "none", no_std)]

#[cfg(feature = "alloc")]
extern crate alloc;

use core::{fmt::Debug, ops::Range};

#[cfg(feature = "alloc")]
mod vec;

mod less;

#[cfg(not(target_os = "none"))]
pub mod test_helper;

pub trait RangeOp: Debug + Clone + Sized + Default {
    type Kind: Debug + Eq + Clone;
    type Type: Ord + Copy;
    fn range(&self) -> Range<Self::Type>;
    fn kind(&self) -> Self::Kind;
    fn overwritable(&self, other: &Self) -> bool;
    fn mergeable(&self, other: &Self) -> bool {
        self.kind() == other.kind()
    }
    fn clone_with_range(&self, range: Range<Self::Type>) -> Self;
}

#[allow(clippy::len_without_is_empty)]
pub trait VecOp<T: RangeOp>: Send + 'static {
    fn push(&mut self, item: T) -> Result<(), RangeError<T>>;
    fn as_slice(&self) -> &[T];
    fn len(&self) -> usize;
    fn remove(&mut self, index: usize) -> T;
    fn insert(&mut self, index: usize, item: T) -> Result<(), RangeError<T>>;

    /// 合并相同类型且相邻或重叠的range
    ///
    /// 此方法会遍历集合，找到所有相同kind且范围相邻或重叠的range，
    /// 并将它们合并成更大的range，以减少集合中的元素数量。
    fn merge_same_kind(&mut self) {
        loop {
            let mut merge_pair: Option<(usize, usize)> = None;

            // 找到第一对可以合并的range
            for i in 0..self.len() {
                for j in (i + 1)..self.len() {
                    let slice = self.as_slice();
                    let current = &slice[i];
                    let next = &slice[j];

                    // 检查是否同类型
                    if current.mergeable(next) {
                        let current_range = current.range();
                        let next_range = next.range();

                        // 检查是否相邻或重叠
                        // 相邻：current.end >= next.start 且 next.end >= current.start
                        if current_range.end >= next_range.start
                            && next_range.end >= current_range.start
                        {
                            merge_pair = Some((i, j));
                            break;
                        }
                    }
                }
                if merge_pair.is_some() {
                    break;
                }
            }

            // 如果找到可以合并的pair，执行合并
            if let Some((i, j)) = merge_pair {
                let slice = self.as_slice();
                let current = &slice[i];
                let next = &slice[j];
                let current_range = current.range();
                let next_range = next.range();

                // 计算合并后的范围
                let new_start = current_range.start.min(next_range.start);
                let new_end = current_range.end.max(next_range.end);
                let merged = current.clone_with_range(new_start..new_end);

                // 删除两个旧的range（先删除索引较大的）
                self.remove(j);
                self.remove(i);
                // 插入合并后的range
                let _ = self.insert(i, merged);
            } else {
                // 没有可以合并的pair了，退出循环
                break;
            }
        }
    }

    fn merge_add(&mut self, item: T) -> Result<(), RangeError<T>> {
        let new_range = item.range();
        let mut i = 0;

        // 遍历现有ranges，处理重叠情况
        while i < self.len() {
            let existing = &self.as_slice()[i];
            let existing_range = existing.range();

            // 检查是否有重叠: new.start < existing.end && new.end > existing.start
            if new_range.start < existing_range.end && new_range.end > existing_range.start {
                // 有重叠，检查是否可覆盖
                if !(existing.overwritable(&item) || existing.mergeable(&item)) {
                    // 不可覆盖，返回冲突错误
                    return Err(RangeError::Conflict {
                        new: item,
                        existing: existing.clone(),
                    });
                }

                // 可覆盖，根据重叠情况处理
                if new_range.start <= existing_range.start && new_range.end >= existing_range.end {
                    // 情况1: 新range完全覆盖旧range，删除旧的
                    self.remove(i);
                    // 不增加i，因为删除后当前位置是下一个元素
                } else if new_range.start > existing_range.start
                    && new_range.end < existing_range.end
                {
                    // 情况2: 新range在旧range中间，分割成两块
                    let left = existing.clone_with_range(existing_range.start..new_range.start);
                    let right = existing.clone_with_range(new_range.end..existing_range.end);

                    self.remove(i);
                    self.insert(i, left)?;
                    self.insert(i + 1, right)?;
                    i += 2; // 跳过刚插入的两个
                } else if new_range.start <= existing_range.start {
                    // 情况3: 新range覆盖旧range的左侧部分，保留右侧
                    let adjusted = existing.clone_with_range(new_range.end..existing_range.end);
                    self.remove(i);
                    self.insert(i, adjusted)?;
                    i += 1;
                } else {
                    // 情况4: 新range覆盖旧range的右侧部分，保留左侧
                    let adjusted = existing.clone_with_range(existing_range.start..new_range.start);
                    self.remove(i);
                    self.insert(i, adjusted)?;
                    i += 1;
                }
            } else {
                i += 1;
            }
        }

        // 添加新的item
        self.push(item)?;
        // 合并相同类型的相邻range
        self.merge_same_kind();
        Ok(())
    }
}

/// RangeSet 错误类型
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RangeError<T>
where
    T: RangeOp,
{
    /// 容量不足错误
    #[error("RangeSet capacity exceeded")]
    Capacity,
    /// 区间冲突错误：尝试覆盖不可覆盖的区间
    #[error("Range conflict: new {new:?} conflicts with existing non-overwritable {existing:?}")]
    Conflict {
        /// 新添加的区间
        new: T,
        /// 已存在的冲突区间
        existing: T,
    },
}
