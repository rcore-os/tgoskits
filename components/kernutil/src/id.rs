/// 内部宏：生成通用的 newtype 实现
#[macro_export]
#[doc(hidden)]
macro_rules! __impl_newtype_common {
    ($name:ident, $inner_type:ty, $display_fmt:literal) => {
        impl $name {
            /// 创建新的值
            #[inline]
            pub const fn new(value: $inner_type) -> Self {
                Self(value)
            }

            /// 获取内部值
            #[inline]
            pub const fn raw(&self) -> $inner_type {
                self.0
            }

            /// 向上对齐到指定边界
            ///
            /// # 参数
            /// - `align`: 对齐边界，必须是 2 的幂
            #[inline]
            pub const fn align_up(self, align: $inner_type) -> Self {
                Self((self.0 + align - 1) & !(align - 1))
            }

            /// 向下对齐到指定边界
            ///
            /// # 参数
            /// - `align`: 对齐边界，必须是 2 的幂
            #[inline]
            pub const fn align_down(self, align: $inner_type) -> Self {
                Self(self.0 & !(align - 1))
            }

            /// 检查是否对齐到指定边界
            ///
            /// # 参数
            /// - `align`: 对齐边界，必须是 2 的幂
            #[inline]
            pub const fn is_aligned_to(self, align: $inner_type) -> bool {
                self.0 & (align - 1) == 0
            }
        }

        impl Default for $name {
            #[inline]
            fn default() -> Self {
                Self(<$inner_type>::default())
            }
        }

        impl core::fmt::Display for $name {
            #[inline]
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(f, $display_fmt, self.0)
            }
        }

        impl core::fmt::Debug for $name {
            #[inline]
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(
                    f,
                    concat!("{}(", $display_fmt, ")"),
                    stringify!($name),
                    self.0
                )
            }
        }

        impl From<$inner_type> for $name {
            #[inline]
            fn from(value: $inner_type) -> Self {
                Self(value)
            }
        }

        impl From<$name> for $inner_type {
            #[inline]
            fn from(id: $name) -> Self {
                id.0
            }
        }

        impl core::ops::Add<$inner_type> for $name {
            type Output = Self;

            #[inline]
            fn add(self, rhs: $inner_type) -> Self::Output {
                Self(self.0 + rhs)
            }
        }

        impl core::ops::Add<$name> for $name {
            type Output = Self;

            #[inline]
            fn add(self, rhs: $name) -> Self::Output {
                Self(self.0 + rhs.0)
            }
        }

        impl core::ops::AddAssign<$inner_type> for $name {
            #[inline]
            fn add_assign(&mut self, rhs: $inner_type) {
                self.0 += rhs;
            }
        }

        impl core::ops::Sub<$inner_type> for $name {
            type Output = Self;

            #[inline]
            fn sub(self, rhs: $inner_type) -> Self::Output {
                Self(self.0 - rhs)
            }
        }

        impl core::ops::Sub<$name> for $name {
            type Output = $inner_type;

            #[inline]
            fn sub(self, rhs: $name) -> $inner_type {
                self.0 - rhs.0
            }
        }

        impl core::ops::SubAssign<$inner_type> for $name {
            #[inline]
            fn sub_assign(&mut self, rhs: $inner_type) {
                self.0 -= rhs;
            }
        }
    };
}

/// 批量定义 newtype 类型
///
/// # 用法
/// ```ignore
/// define_type! {
///     /// 任务 ID
///     TaskId(usize),
///     /// CPU ID
///     CpuId(u32),
///     /// 物理地址（自定义十六进制格式）
///     PhysAddr(usize, "{:#x}"),
/// }
/// ```
#[macro_export]
macro_rules! define_type {
    // 内部规则：处理带格式的类型
    (@item $(#[$meta:meta])* $name:ident($inner_type:ty, $fmt:literal)) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(transparent)]
        pub struct $name($inner_type);

        $crate::__impl_newtype_common!($name, $inner_type, $fmt);
    };
    // 内部规则：处理不带格式的类型（默认十进制）
    (@item $(#[$meta:meta])* $name:ident($inner_type:ty)) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(transparent)]
        pub struct $name($inner_type);

        $crate::__impl_newtype_common!($name, $inner_type, "{}");
    };
    // 主入口：匹配多个类型定义
    (
        $(
            $(#[$meta:meta])*
            $name:ident($inner_type:ty $(, $fmt:literal)?)
        ),* $(,)?
    ) => {
        $(
            $crate::define_type!(@item $(#[$meta])* $name($inner_type $(, $fmt)?));
        )*
    };
}

#[cfg(test)]
mod tests {

    // 使用 define_type! 批量定义测试用类型
    define_type! {
        /// 测试用 ID
        TestId(usize),
        /// 支持负数的 ID
        NegId(isize),
        /// 测试地址（十六进制）
        TestAddr(usize, "{:#x}"),
    }

    #[test]
    fn test_basic_id_creation() {
        let id = TestId::new(42);
        assert_eq!(id.raw(), 42);
    }

    #[test]
    fn test_arithmetic_operations() {
        let id = TestId::new(10);

        // 加法
        assert_eq!((id + 5).raw(), 15);
        assert_eq!((id + TestId::new(3)).raw(), 13);

        // 减法
        assert_eq!((id - 3).raw(), 7);
        assert_eq!(id - TestId::new(2), 8);
    }

    #[test]
    fn test_assignment_operations() {
        let mut id = TestId::new(10);

        id += 5;
        assert_eq!(id.raw(), 15);

        id -= 3;
        assert_eq!(id.raw(), 12);
    }

    #[test]
    fn test_comparisons() {
        let id1 = TestId::new(10);
        let id2 = TestId::new(20);
        let id3 = TestId::new(10);

        assert_eq!(id1, id3);
        assert_ne!(id1, id2);
        assert!(id1 < id2);
        assert!(id2 > id1);
        assert!(id1 <= id3);
        assert!(id1 >= id3);
    }

    #[test]
    fn test_alignment() {
        let addr = TestAddr::new(0x1234);

        // align_up
        assert_eq!(addr.align_up(0x1000).raw(), 0x2000);
        assert_eq!(TestAddr::new(0x1000).align_up(0x1000).raw(), 0x1000);
        assert_eq!(TestAddr::new(0x1001).align_up(0x1000).raw(), 0x2000);

        // align_down
        assert_eq!(addr.align_down(0x1000).raw(), 0x1000);
        assert_eq!(TestAddr::new(0x1000).align_down(0x1000).raw(), 0x1000);
        assert_eq!(TestAddr::new(0x1FFF).align_down(0x1000).raw(), 0x1000);

        // is_aligned_to
        assert!(TestAddr::new(0x1000).is_aligned_to(0x1000));
        assert!(TestAddr::new(0x2000).is_aligned_to(0x1000));
        assert!(!TestAddr::new(0x1001).is_aligned_to(0x1000));
        assert!(!TestAddr::new(0x1234).is_aligned_to(0x1000));
    }
}
