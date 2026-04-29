#[derive(Debug, Clone, Copy)]
pub struct GrfMmio {
    pub base: usize,
    pub size: usize,
}

/// 批量定义 GRF 寄存器常量的宏
///
/// # 使用方式
///
/// ```
/// define_grf!(
///     // 单个定义：名称, 基地址, 大小
///     GRF0, 0xff770000, 0x1000;
///
///     // 批量定义：每个定义用分号分隔
///     GRF1, 0xff780000, 0x1000;
///     GRF2, 0xff790000, 0x1000;
/// );
/// ```
macro_rules! define_grf {
    // 批量定义入口：递归处理每个分号分隔的定义
    ($($name:ident, $base:expr, $size:expr);+ $(;)?) => {
        $(
            #[allow(unused)]
            pub const $name: $crate::grf::GrfMmio =
                $crate::grf::GrfMmio { base: $base, size: $size };
        )+
    };


}
