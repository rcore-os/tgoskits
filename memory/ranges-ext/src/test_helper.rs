use core::ops::Range;

use crate::RangeOp;

/// 测试用的 Range 类型
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestRange {
    pub start: u64,
    pub end: u64,
    pub kind: RangeKind,
    pub overwritable: bool,
}

impl Default for TestRange {
    fn default() -> Self {
        Self {
            start: 0,
            end: 0,
            kind: RangeKind::default(),
            overwritable: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RangeKind {
    #[default]
    TypeA,
    TypeB,
    TypeC,
}

impl TestRange {
    pub fn new(start: u64, end: u64, kind: RangeKind) -> Self {
        Self {
            start,
            end,
            kind,
            overwritable: true,
        }
    }

    pub fn new_with_overwritable(
        start: u64,
        end: u64,
        kind: RangeKind,
        overwritable: bool,
    ) -> Self {
        Self {
            start,
            end,
            kind,
            overwritable,
        }
    }
}

impl RangeOp for TestRange {
    type Kind = RangeKind;
    type Type = u64;

    fn range(&self) -> Range<Self::Type> {
        self.start..self.end
    }

    fn kind(&self) -> Self::Kind {
        self.kind.clone()
    }

    fn overwritable(&self, _other: &Self) -> bool {
        self.overwritable
    }

    fn clone_with_range(&self, range: Range<Self::Type>) -> Self {
        Self {
            start: range.start,
            end: range.end,
            kind: self.kind.clone(),
            overwritable: self.overwritable,
        }
    }
}
