//! RK3588 PLL 时钟配置
//!
//! 参考 u-boot-orangepi/drivers/clk/rockchip/clk_rk3588.c

use super::{ClockError, ClockResult, Cru, consts::*};
use crate::clock::{ClkId, pll::*};

/// PLL 模式掩码
const PLL_MODE_MASK: u32 = 0x3;

/// RK3588 PLL 时钟 ID
///
/// 对应 u-boot 中的 enum rk3588_pll_id (cru_rk3588.h:22)
/// 值与 ClkId 保持一致
#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum PllId {
    /// BIGCORE0 PLL - 大核0 PLL
    B0PLL = 1,
    /// BIGCORE1 PLL - 大核1 PLL
    B1PLL = 2,
    /// DSU PLL - 小核共享单元 PLL
    LPLL  = 3,
    /// 视频 PLL
    V0PLL = 4,
    /// 音频 PLL
    AUPLL = 5,
    /// 中心/通用 PLL
    CPLL  = 6,
    /// 通用 PLL
    GPLL  = 7,
    /// 网络/视频 PLL
    NPLL  = 8,
    /// PMU PLL
    PPLL  = 9,
}

// =============================================================================
// PllId 与 ClkId 的双向转换
// =============================================================================

impl From<PllId> for ClkId {
    fn from(pll_id: PllId) -> Self {
        ClkId::new(pll_id as u64)
    }
}

impl TryFrom<ClkId> for PllId {
    type Error = &'static str;

    fn try_from(clk_id: ClkId) -> Result<Self, Self::Error> {
        match clk_id.value() {
            1 => Ok(PllId::B0PLL),
            2 => Ok(PllId::B1PLL),
            3 => Ok(PllId::LPLL),
            4 => Ok(PllId::V0PLL),
            5 => Ok(PllId::AUPLL),
            6 => Ok(PllId::CPLL),
            7 => Ok(PllId::GPLL),
            8 => Ok(PllId::NPLL),
            9 => Ok(PllId::PPLL),
            _ => Err("Invalid PLL clock ID"),
        }
    }
}

impl PllId {
    /// 获取 PLL 名称
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::B0PLL => "B0PLL",
            Self::B1PLL => "B1PLL",
            Self::LPLL => "LPLL",
            Self::V0PLL => "V0PLL",
            Self::AUPLL => "AUPLL",
            Self::CPLL => "CPLL",
            Self::GPLL => "GPLL",
            Self::NPLL => "NPLL",
            Self::PPLL => "PPLL",
        }
    }

    /// 获取 PLL 默认频率 (Hz)
    ///
    /// 参考 cru_rk3588.h:15-19
    #[must_use]
    pub const fn default_rate(&self) -> Option<u64> {
        match self {
            Self::B0PLL | Self::B1PLL => Some(LPLL_HZ),
            Self::LPLL => Some(LPLL_HZ),
            Self::GPLL => Some(GPLL_HZ),
            Self::CPLL => Some(CPLL_HZ),
            Self::NPLL => Some(NPLL_HZ),
            Self::PPLL => Some(PPLL_HZ),
            _ => None,
        }
    }
}

/// RK3588 PLL 预设频率表
///
/// 参考 clk_rk3588.c:24
///
/// 支持的频率范围: 100MHz - 1.5GHz
pub const PLL_RATE_TABLE: &[PllRateTable] = &[
    pll_rate(1500000000, 2, 250, 1, 0),
    pll_rate(1200000000, 2, 200, 1, 0),
    pll_rate(1188000000, 2, 198, 1, 0),
    pll_rate(1100000000, 3, 550, 2, 0),
    pll_rate(1008000000, 2, 336, 2, 0),
    pll_rate(1000000000, 3, 500, 2, 0),
    pll_rate(900000000, 2, 300, 2, 0),
    pll_rate(850000000, 3, 425, 2, 0),
    pll_rate(816000000, 2, 272, 2, 0),
    pll_rate(786432000, 2, 262, 2, 9437),
    pll_rate(786000000, 1, 131, 2, 0),
    pll_rate(742500000, 4, 495, 2, 0),
    pll_rate(722534400, 8, 963, 2, 24850),
    pll_rate(600000000, 2, 200, 2, 0),
    pll_rate(594000000, 2, 198, 2, 0),
    pll_rate(200000000, 3, 400, 4, 0),
    pll_rate(100000000, 3, 400, 5, 0),
];

macro_rules! pll {
    ($id:ident, $con:expr, $mode:expr, $mshift:expr, $lshift:expr, $pflags:expr) => {
        PllClock {
            // 时钟 ID: 从 1 开始 (匹配设备树绑定 rk3588-cru.h)
            id: PllId::$id as u32,
            con_offset: $con,
            mode_offset: $mode,
            mode_shift: $mshift,
            lock_shift: $lshift,
            pll_type: RockchipPllType::Rk3588,
            pll_flags: $pflags,
            rate_table: PLL_RATE_TABLE,
            mode_mask: 0,
        }
    };
}

/// RK3588 PLL 时钟配置
///
/// 参考 u-boot-orangepi/drivers/clk/rockchip/clk_rk3588.c:46
///
/// RK3588 共有 9 个 PLL:
/// - B0PLL/B1PLL: 大核 PLL (BIGCORE0/1)
/// - LPLL: 小核 PLL (DSU)
/// - V0PLL: 视频 PLL
/// - AUPLL: 音频 PLL
/// - CPLL: 中心/通用 PLL
/// - GPLL: 通用 PLL
/// - NPLL: 网络/视频 PLL
/// - PPLL: PMU PLL
///
/// 注意: 数组顺序必须与 PllId 枚举顺序一致
const RK3588_PLL_CLOCKS: [PllClock; 9] = [
    // [0] B0PLL - BIGCORE0 PLL (偏移 0x50000)
    pll!(B0PLL, b0_pll_con(0), RK3588_B0_PLL_MODE_CON, 0, 15, 0),
    // [1] B1PLL - BIGCORE1 PLL (偏移 0x52000)
    pll!(B1PLL, b1_pll_con(8), RK3588_B1_PLL_MODE_CON, 0, 15, 0),
    // [2] LPLL - DSU PLL (偏移 0x58000)
    pll!(LPLL, lpll_con(16), RK3588_LPLL_MODE_CON, 0, 15, 0),
    // [3] V0PLL - 视频 PLL (偏移 0x160)
    pll!(V0PLL, pll_con(88), RK3588_MODE_CON0, 4, 15, 0),
    // [4] AUPLL - 音频 PLL (偏移 0x180)
    pll!(AUPLL, pll_con(96), RK3588_MODE_CON0, 6, 15, 0),
    // [5] CPLL - 中心/通用 PLL (偏移 0x1a0)
    pll!(CPLL, pll_con(104), RK3588_MODE_CON0, 8, 15, 0),
    // [6] GPLL - 通用 PLL (偏移 0x1c0)
    pll!(GPLL, pll_con(112), RK3588_MODE_CON0, 2, 15, 0),
    // [7] NPLL - 网络/视频 PLL (偏移 0x1e0)
    pll!(NPLL, pll_con(120), RK3588_MODE_CON0, 0, 15, 0),
    // [8] PPLL - PMU PLL (偏移 0x8000)
    pll!(PPLL, pmu_pll_con(128), RK3588_MODE_CON0, 10, 15, 0),
];

impl Cru {
    /// 读取 PLL 实际频率
    ///
    /// 参考 u-boot: drivers/clk/rockchip/clk_pll.c:rk3588_pll_get_rate()
    ///
    /// # 参数
    ///
    /// * `pll_id` - PLL ID
    ///
    /// # 返回
    ///
    /// PLL 输出频率 (Hz)
    pub(crate) fn pll_get_rate(&self, pll_id: PllId) -> ClockResult<u64> {
        let pll_cfg = get_pll(pll_id);

        // 1. 读取 PLL 模式
        let mode_con = self.read(pll_cfg.mode_offset);
        let mode_shift = pll_cfg.mode_shift;

        // PPLL (ID=8) 特殊处理: 始终认为是 NORMAL 模式
        let pll_id_val = pll_id as u32;
        let mode = if pll_id_val == 8 {
            pll_mode::PLL_MODE_NORMAL
        } else {
            (mode_con & (PLL_MODE_MASK << mode_shift)) >> mode_shift
        };

        match mode {
            pll_mode::PLL_MODE_SLOW => {
                debug!(
                    "{}[mode_shift={}] is in SLOW mode, returning OSC_HZ",
                    pll_id.name(),
                    mode_shift
                );
                return Ok(OSC_HZ);
            }
            pll_mode::PLL_MODE_DEEP => {
                debug!(
                    "{}[mode_shift={}] is in DEEP mode, returning 32768Hz",
                    pll_id.name(),
                    mode_shift
                );
                return Ok(32768);
            }
            pll_mode::PLL_MODE_NORMAL => {
                // 继续读取 PLL 参数
            }
            _ => {
                log::warn!(
                    "⚠️ {}[mode_shift={}]: unknown mode={}, returning 0",
                    pll_id.name(),
                    mode_shift,
                    mode
                );
                return Ok(0);
            }
        }

        // 2. 读取 PLL 参数 (参考 u-boot rk3588_pll_get_rate)
        // PLLCON0: M (10 bits)
        let con0 = self.read(pll_cfg.con_offset);
        let m = (con0 & pllcon0::M_MASK) >> pllcon0::M_SHIFT;

        // PLLCON1: P (6 bits), S (3 bits)
        let con1 = self.read(pll_cfg.con_offset + pll_con(1));
        let p = (con1 & pllcon1::P_MASK) >> pllcon1::P_SHIFT;
        let s = (con1 & pllcon1::S_MASK) >> pllcon1::S_SHIFT;

        // PLLCON2: K (16 bits)
        let con2 = self.read(pll_cfg.con_offset + pll_con(2));
        let k = (con2 & pllcon2::K_MASK) >> pllcon2::K_SHIFT;

        debug!("{}: p={}, m={}, s={}, k={}", pll_id.name(), p, m, s, k);

        // 3. 验证 p 值
        if p == 0 {
            log::warn!(
                "⚠️ PLL[mode_shift={}] has invalid p=0, assuming not configured, returning OSC_HZ",
                mode_shift
            );
            return Ok(OSC_HZ);
        }

        // 4. 计算频率 (参考 u-boot rk3588_pll_get_rate)
        // rate = OSC_HZ / p * m
        let mut rate: u64 = (OSC_HZ / p as u64) * m as u64;

        // 如果有小数分频 k
        if k != 0 {
            // frac_rate = OSC_HZ * k / (p * 65536)
            let frac_rate = (OSC_HZ * k as u64) / (p as u64 * 65536);
            rate += frac_rate;
        }

        // 右移 s 位 (后分频)
        rate >>= s;

        debug!("{}: calculated rate = {}MHz", pll_id.name(), rate / MHZ);

        Ok(rate)
    }

    /// 设置 PLL 频率
    ///
    /// 参考 u-boot: drivers/clk/rockchip/clk_pll.c:rk3588_pll_set_rate()
    ///
    /// # 参数
    ///
    /// * `pll_id` - PLL ID
    /// * `rate_hz` - 目标频率 (Hz)
    ///
    /// # 返回
    ///
    /// 成功返回 Ok(实际频率), 失败返回 Err
    ///
    /// # 配置流程
    ///
    /// 1. 查找频率表或计算参数
    /// 2. 切换到 SLOW 模式
    /// 3. Power down PLL
    /// 4. 写入 PLL 参数 (p, m, s, k)
    /// 5. Power up PLL
    /// 6. 等待 PLL 锁定
    /// 7. 切换到 NORMAL 模式
    pub fn pll_set_rate(&mut self, pll_id: PllId, rate_hz: u64) -> ClockResult<u64> {
        let pll_cfg = get_pll(pll_id);

        info!(
            "CRU@{:x}: Setting {} to {}MHz...",
            self.base,
            pll_id.name(),
            rate_hz / MHZ
        );

        // ========================================================================
        // 1. 查找或计算 PLL 参数 (p, m, s, k)
        // ========================================================================
        let (p, m, s, k) = find_pll_params(pll_id, rate_hz).map_err(|e| {
            ClockError::pll_config_error(crate::clock::ClkId::from(pll_id as u32), e)
        })?;

        debug!(
            "{}: calculated params: p={}, m={}, s={}, k={}",
            pll_id.name(),
            p,
            m,
            s,
            k
        );

        // ========================================================================
        // 2. 切换到 SLOW 模式
        // u-boot: rk_clrsetreg(base + pll->mode_offset,
        //                      pll->mode_mask << pll->mode_shift,
        //                      RKCLK_PLL_MODE_SLOW << pll->mode_shift);
        // ========================================================================
        self.clrsetreg(
            pll_cfg.mode_offset,
            PLL_MODE_MASK << pll_cfg.mode_shift,
            pll_mode::PLL_MODE_SLOW << pll_cfg.mode_shift,
        );

        debug!("{}: switched to SLOW mode", pll_id.name());

        // ========================================================================
        // 3. Power down PLL
        // u-boot: rk_setreg(base + pll->con_offset + RK3588_PLLCON(1),
        //                   RK3588_PLLCON1_PWRDOWN);
        // ========================================================================
        self.setreg(pll_cfg.con_offset + pll_con(1), pllcon1::PWRDOWN);

        // ========================================================================
        // 4. 写入 PLL 参数
        // u-boot: rk_clrsetreg(base + pll->con_offset, RK3588_PLLCON0_M_MASK,
        //                      rate->m << RK3588_PLLCON0_M_SHIFT);
        // ========================================================================

        // 写入 M (10 bits)
        self.clrsetreg(pll_cfg.con_offset, pllcon0::M_MASK, m << pllcon0::M_SHIFT);

        // 写入 P (6 bits) 和 S (3 bits)
        self.clrsetreg(
            pll_cfg.con_offset + pll_con(1),
            pllcon1::P_MASK | pllcon1::S_MASK,
            (p << pllcon1::P_SHIFT) | (s << pllcon1::S_SHIFT),
        );

        // 写入 K (16 bits, 如果有小数分频)
        if k != 0 {
            self.clrsetreg(
                pll_cfg.con_offset + pll_con(2),
                pllcon2::K_MASK,
                k << pllcon2::K_SHIFT,
            );
        }

        debug!("{}: PLL parameters written", pll_id.name());

        // ========================================================================
        // 5. Power up PLL
        // u-boot: rk_clrreg(base + pll->con_offset + RK3588_PLLCON(1),
        //                   RK3588_PLLCON1_PWRDOWN);
        // ========================================================================
        self.clrreg(pll_cfg.con_offset + pll_con(1), pllcon1::PWRDOWN);

        // ========================================================================
        // 6. 等待 PLL 锁定
        // u-boot: while (!(readl(base + pll->con_offset + RK3588_PLLCON(6)) &
        //                  RK3588_PLLCON6_LOCK_STATUS)) {
        //             udelay(1);
        //         }
        // ========================================================================
        let mut timeout = 1000; // 1ms timeout (1000 * 1us)
        let con6_addr = pll_cfg.con_offset + pll_con(6);

        while self.read(con6_addr) & pllcon6::LOCK_STATUS == 0 {
            if timeout == 0 {
                log::error!("⚠️ {}: PLL lock timeout!", pll_id.name());
                return Err(ClockError::pll_config_error(
                    crate::clock::ClkId::from(pll_id as u32),
                    "PLL lock timeout",
                ));
            }
            // 简单延迟循环 (裸机环境)
            for _ in 0..100 {
                core::hint::spin_loop();
            }
            timeout -= 1;
        }

        debug!(
            "{}: PLL locked after {} attempts",
            pll_id.name(),
            1000 - timeout
        );

        // ========================================================================
        // 7. 切换到 NORMAL 模式
        // u-boot: rk_clrsetreg(base + pll->mode_offset,
        //                      pll->mode_mask << pll->mode_shift,
        //                      RKCLK_PLL_MODE_NORMAL << pll->mode_shift);
        // ========================================================================
        self.clrsetreg(
            pll_cfg.mode_offset,
            PLL_MODE_MASK << pll_cfg.mode_shift,
            pll_mode::PLL_MODE_NORMAL << pll_cfg.mode_shift,
        );

        debug!("{}: switched to NORMAL mode", pll_id.name());

        // ========================================================================
        // 8. 验证实际输出频率
        // ========================================================================
        let actual_rate = self.pll_get_rate(pll_id)?;

        log::info!(
            "✓ CRU@{:x}: {} set to {}MHz (requested: {}MHz)",
            self.base,
            pll_id.name(),
            actual_rate / MHZ,
            rate_hz / MHZ
        );

        Ok(actual_rate)
    }
}

/// 创建 RK3588 PLL 速率表项
///
/// # 参数
///
/// * `rate` - 目标输出频率 (Hz)
/// * `p` - P 分频系数 (Pre-divider)
/// * `m` - M 分频系数 (Main Divider)
/// * `s` - S 分频系数 (Post-divider)
/// * `k` - K 小数分频系数
///
/// # 示例
///
/// ```rust
/// let rate = pll_rate(1188_000_000, 2, 198, 1, 0);
/// ```
const fn pll_rate(rate: u64, p: u32, m: u32, s: u32, k: u32) -> PllRateTable {
    PllRateTable {
        rate,
        params: PllRateParams::Rk3588 { p, m, s, k },
    }
}

/// 通过 ID 获取 PLL 配置
///
/// # 参数
///
/// * `id` - PLL ID
///
/// # 返回
///
/// 返回对应 PLL 的配置引用
pub const fn get_pll(id: PllId) -> &'static PllClock {
    &RK3588_PLL_CLOCKS[id as usize - 1]
}

/// 计算 RK3588 PLL 输出频率
///
/// # 公式
///
/// 参考 u-boot clk_pll.c 的 rk3588_pll_get_rate():
///
/// ```text
/// rate = (OSC_HZ / p) * m
/// if (k):
///     frac_rate = (OSC_HZ * k) / (p * 65536)
///     rate = rate + frac_rate
/// rate = rate >> s
/// ```
///
/// 等价于:
/// ```text
/// FOUT = ((FIN / P) * M + (FIN * K) / (P * 65536)) >> S
/// FOUT = ((FIN * M) / P + (FIN * K) / (P * 65536)) / 2^S
/// ```
///
/// # 参数
///
/// * `fin` - 输入频率 (Hz), 通常为 24MHz
/// * `p` - P 分频系数
/// * `m` - M 分频系数
/// * `s` - S 分频系数 (作为右移位数)
/// * `k` - K 小数分频系数
///
/// # 返回
///
/// 计算得到的输出频率 (Hz)
#[must_use]
pub const fn calc_pll_rate(fin: u64, p: u32, m: u32, s: u32, k: u32) -> u64 {
    let p = p as u64;
    let m = m as u64;

    if k != 0 {
        // 小数分频模式 - 参考 u-boot clk_pll.c 的 rk3588_pll_get_rate()
        let rate = (fin / p) * m;
        let frac_rate = (fin * k as u64) / (p * 65536);
        (rate + frac_rate) >> s
    } else {
        // 整数分频模式
        ((fin / p) * m) >> s
    }
}

/// 查找或计算 PLL 参数
///
/// # 参数
///
/// * `pll_id` - PLL ID
/// * `rate_hz` - 目标频率 (Hz)
///
/// # 返回
///
/// (p, m, s, k) 参数元组
pub fn find_pll_params(pll_id: PllId, rate_hz: u64) -> Result<(u32, u32, u32, u32), &'static str> {
    let pll_cfg = get_pll(pll_id);

    // 1. 首先尝试从预设频率表中查找
    for entry in pll_cfg.rate_table {
        if entry.rate == rate_hz
            && let PllRateParams::Rk3588 { p, m, s, k } = entry.params
        {
            debug!(
                "{}: found preset rate table entry for {}MHz",
                pll_id.name(),
                rate_hz / MHZ
            );
            return Ok((p, m, s, k));
        }
    }

    // 2. 如果预设表没有,尝试简单计算 (仅支持整数分频)
    // 公式: fout = ((fin / p) * m) >> s
    // 简化: 设 p=2, s=1, 则 fout = (fin / 2 * m) >> 1 = fin * m / 4
    // 因此: m = fout * 4 / fin

    let fin = OSC_HZ;
    let target_vco = rate_hz * 4; // 假设 s=2 (后分频4)

    // 检查 VCO 频率范围
    const VCO_MIN_HZ: u64 = 2250 * MHZ;
    const VCO_MAX_HZ: u64 = 4500 * MHZ;

    if !(VCO_MIN_HZ..=VCO_MAX_HZ).contains(&target_vco) {
        return Err("Target frequency out of VCO range");
    }

    // 计算参数: p=2, s=2 (后分频4)
    let p = 2u32;
    let s = 2u32;
    let m = ((rate_hz << s) / (fin / p as u64)) as u32;
    let k = 0u32; // 暂不支持小数分频计算

    // 验证计算结果
    let check_rate = calc_pll_rate(fin, p, m, s, k);
    let tolerance = rate_hz / 1000; // 0.1% 容差

    if check_rate.abs_diff(rate_hz) > tolerance {
        return Err("Cannot calculate accurate PLL parameters");
    }

    log::warn!(
        "⚠️ {}: No preset rate table entry for {}MHz, calculated: p={}, m={}, s={}, k={}",
        pll_id.name(),
        rate_hz / MHZ,
        p,
        m,
        s,
        k
    );

    Ok((p, m, s, k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pll_rate_table_count() {
        // 验证频率表项数量正确 (17 项)
        assert_eq!(PLL_RATE_TABLE.len(), 17);
    }

    #[test]
    fn test_pll_rate_calculation() {
        // 测试整数分频
        // fin=24MHz, p=2, m=198, s=1, k=0 => ((24/2)*198) >> 1 = 1188MHz
        let rate = calc_pll_rate(24_000_000, 2, 198, 1, 0);
        assert_eq!(rate, 1_188_000_000);

        // 测试小数分频
        // 参考 clk_rk3588.c:35 - 目标 786.432MHz
        // 由于整数除法精度限制,实际计算值为 786431991 Hz
        // 公式: rate = (24MHz/2)*262 + (24MHz*9437)/(2*65536) = 3144000000 + 1727966
        //       result = (3144000000 + 1727966) >> 2 = 786431991
        let rate = calc_pll_rate(24_000_000, 2, 262, 2, 9437);
        assert_eq!(rate, 786_431_991);
    }

    #[test]
    fn test_pll_count() {
        // RK3588 应该有 9 个 PLL
        assert_eq!(RK3588_PLL_CLOCKS.len(), 9);
    }

    #[test]
    fn test_pll_ids() {
        // 验证 PLL ID 值 (匹配设备树绑定 rk3588-cru.h)
        assert_eq!(PllId::B0PLL as u64, 1);
        assert_eq!(PllId::B1PLL as u64, 2);
        assert_eq!(PllId::LPLL as u64, 3);
        assert_eq!(PllId::V0PLL as u64, 4);
        assert_eq!(PllId::AUPLL as u64, 5);
        assert_eq!(PllId::CPLL as u64, 6);
        assert_eq!(PllId::GPLL as u64, 7);
        assert_eq!(PllId::NPLL as u64, 8);
        assert_eq!(PllId::PPLL as u64, 9);
    }

    #[test]
    fn test_pll_names() {
        assert_eq!(PllId::GPLL.name(), "GPLL");
        assert_eq!(PllId::CPLL.name(), "CPLL");
        assert_eq!(PllId::NPLL.name(), "NPLL");
    }

    #[test]
    fn test_pll_default_rates() {
        assert_eq!(PllId::GPLL.default_rate(), Some(GPLL_HZ as u64));
        assert_eq!(PllId::CPLL.default_rate(), Some(CPLL_HZ as u64));
        assert_eq!(PllId::NPLL.default_rate(), Some(NPLL_HZ as u64));
    }

    #[test]
    fn test_pll_config_offsets() {
        // 验证关键 PLL 的寄存器偏移
        let gpll = get_pll(PllId::GPLL);
        assert_eq!(gpll.con_offset, pll_con(112));
        assert_eq!(gpll.mode_offset, RK3588_MODE_CON0);
        assert_eq!(gpll.mode_shift, 2);

        let cpll = get_pll(PllId::CPLL);
        assert_eq!(cpll.con_offset, pll_con(104));
        assert_eq!(cpll.mode_offset, RK3588_MODE_CON0);
        assert_eq!(cpll.mode_shift, 8);
    }

    // ========================================================================
    // PLL 配置值完整验证 - 对比 u-boot clk_rk3588.c:46
    // ========================================================================

    #[test]
    fn test_b0pll_config() {
        // 验证 B0PLL 配置
        // 对应 C 代码: PLL(pll_rk3588, PLL_B0PLL, RK3588_B0_PLL_CON(0), RK3588_B0_PLL_MODE_CON, 0, 15, 0, ...)
        let pll = get_pll(PllId::B0PLL);

        // 验证 ID (匹配设备树绑定 rk3588-cru.h: PLL_B0PLL = 1)
        assert_eq!(pll.id, 1, "B0PLL ID should be 1");

        // 验证寄存器偏移: RK3588_B0_PLL_CON(0) = 0 * 0x4 + 0x50000 = 0x50000
        assert_eq!(
            pll.con_offset, 0x50000,
            "B0PLL con_offset should be 0x50000"
        );

        // 验证模式寄存器偏移: RK3588_B0_PLL_MODE_CON = 0x50000 + 0x280
        assert_eq!(
            pll.mode_offset, 0x50280,
            "B0PLL mode_offset should be 0x50280"
        );

        // 验证位移和锁定位
        assert_eq!(pll.mode_shift, 0, "B0PLL mode_shift should be 0");
        assert_eq!(pll.lock_shift, 15, "B0PLL lock_shift should be 15");

        // 验证 PLL 类型
        assert_eq!(pll.pll_type, RockchipPllType::Rk3588);

        // 验证标志位
        assert_eq!(pll.pll_flags, 0);

        // 验证模式掩码
        assert_eq!(pll.mode_mask, 0);

        // 验证频率表引用
        assert_eq!(pll.rate_table.len(), 17);
    }

    #[test]
    fn test_b1pll_config() {
        // 验证 B1PLL 配置
        // 对应 C 代码: PLL(pll_rk3588, PLL_B1PLL, RK3588_B1_PLL_CON(8), RK3588_B1_PLL_MODE_CON, 0, 15, 0, ...)
        let pll = get_pll(PllId::B1PLL);

        assert_eq!(pll.id, 2, "B1PLL ID should be 2");

        // RK3588_B1_PLL_CON(8) = 8 * 0x4 + 0x52000 = 0x52020
        assert_eq!(
            pll.con_offset, 0x52020,
            "B1PLL con_offset should be 0x52020"
        );

        // RK3588_B1_PLL_MODE_CON = 0x52000 + 0x280 = 0x52280
        assert_eq!(
            pll.mode_offset, 0x52280,
            "B1PLL mode_offset should be 0x52280"
        );

        assert_eq!(pll.mode_shift, 0);
        assert_eq!(pll.lock_shift, 15);
    }

    #[test]
    fn test_lpll_config() {
        // 验证 LPLL 配置
        // 对应 C 代码: PLL(pll_rk3588, PLL_LPLL, RK3588_LPLL_CON(16), RK3588_LPLL_MODE_CON, 0, 15, 0, ...)
        let pll = get_pll(PllId::LPLL);

        assert_eq!(pll.id, 3, "LPLL ID should be 3");

        // RK3588_LPLL_CON(16) = 16 * 0x4 + 0x58000 = 0x58040
        assert_eq!(pll.con_offset, 0x58040, "LPLL con_offset should be 0x58040");

        // RK3588_LPLL_MODE_CON = 0x58000 + 0x280 = 0x58280
        assert_eq!(
            pll.mode_offset, 0x58280,
            "LPLL mode_offset should be 0x58280"
        );

        assert_eq!(pll.mode_shift, 0);
        assert_eq!(pll.lock_shift, 15);
    }

    #[test]
    fn test_v0pll_config() {
        // 验证 V0PLL 配置
        // 对应 C 代码: PLL(pll_rk3588, PLL_V0PLL, RK3588_PLL_CON(88), RK3588_MODE_CON0, 4, 15, 0, ...)
        let pll = get_pll(PllId::V0PLL);

        assert_eq!(pll.id, 4, "V0PLL ID should be 4");

        // RK3588_PLL_CON(88) = 88 * 0x4 = 0x160
        assert_eq!(pll.con_offset, 0x160, "V0PLL con_offset should be 0x160");

        // RK3588_MODE_CON0 = 0x280
        assert_eq!(pll.mode_offset, 0x280, "V0PLL mode_offset should be 0x280");

        assert_eq!(pll.mode_shift, 4, "V0PLL mode_shift should be 4");
        assert_eq!(pll.lock_shift, 15);
    }

    #[test]
    fn test_aupll_config() {
        // 验证 AUPLL 配置
        // 对应 C 代码: PLL(pll_rk3588, PLL_AUPLL, RK3588_PLL_CON(96), RK3588_MODE_CON0, 6, 15, 0, ...)
        let pll = get_pll(PllId::AUPLL);

        assert_eq!(pll.id, 5, "AUPLL ID should be 5");

        // RK3588_PLL_CON(96) = 96 * 0x4 = 0x180
        assert_eq!(pll.con_offset, 0x180, "AUPLL con_offset should be 0x180");

        assert_eq!(pll.mode_offset, 0x280);
        assert_eq!(pll.mode_shift, 6, "AUPLL mode_shift should be 6");
        assert_eq!(pll.lock_shift, 15);
    }

    #[test]
    fn test_cpll_config() {
        // 验证 CPLL 配置
        // 对应 C 代码: PLL(pll_rk3588, PLL_CPLL, RK3588_PLL_CON(104), RK3588_MODE_CON0, 8, 15, 0, ...)
        let pll = get_pll(PllId::CPLL);

        assert_eq!(pll.id, 6, "CPLL ID should be 6");

        // RK3588_PLL_CON(104) = 104 * 0x4 = 0x1a0
        assert_eq!(pll.con_offset, 0x1a0, "CPLL con_offset should be 0x1a0");

        assert_eq!(pll.mode_offset, 0x280);
        assert_eq!(pll.mode_shift, 8, "CPLL mode_shift should be 8");
        assert_eq!(pll.lock_shift, 15);
    }

    #[test]
    fn test_gpll_config() {
        // 验证 GPLL 配置
        // 对应 C 代码: PLL(pll_rk3588, PLL_GPLL, RK3588_PLL_CON(112), RK3588_MODE_CON0, 2, 15, 0, ...)
        let pll = get_pll(PllId::GPLL);

        assert_eq!(pll.id, 7, "GPLL ID should be 7");

        // RK3588_PLL_CON(112) = 112 * 0x4 = 0x1c0
        assert_eq!(pll.con_offset, 0x1c0, "GPLL con_offset should be 0x1c0");

        assert_eq!(pll.mode_offset, 0x280);
        assert_eq!(pll.mode_shift, 2, "GPLL mode_shift should be 2");
        assert_eq!(pll.lock_shift, 15);
    }

    #[test]
    fn test_npll_config() {
        // 验证 NPLL 配置
        // 对应 C 代码: PLL(pll_rk3588, PLL_NPLL, RK3588_PLL_CON(120), RK3588_MODE_CON0, 0, 15, 0, ...)
        let pll = get_pll(PllId::NPLL);

        assert_eq!(pll.id, 8, "NPLL ID should be 8");

        // RK3588_PLL_CON(120) = 120 * 0x4 = 0x1e0
        assert_eq!(pll.con_offset, 0x1e0, "NPLL con_offset should be 0x1e0");

        assert_eq!(pll.mode_offset, 0x280);
        assert_eq!(pll.mode_shift, 0, "NPLL mode_shift should be 0");
        assert_eq!(pll.lock_shift, 15);
    }

    #[test]
    fn test_ppll_config() {
        // 验证 PPLL 配置
        // 对应 C 代码: PLL(pll_rk3588, PLL_PPLL, RK3588_PMU_PLL_CON(128), RK3588_MODE_CON0, 10, 15, 0, ...)
        let pll = get_pll(PllId::PPLL);

        assert_eq!(pll.id, 9, "PPLL ID should be 9");

        // RK3588_PMU_PLL_CON(128) = 128 * 0x4 + 0x8000 = 0x8200
        assert_eq!(pll.con_offset, 0x8200, "PPLL con_offset should be 0x8200");

        assert_eq!(pll.mode_offset, 0x280);
        assert_eq!(pll.mode_shift, 10, "PPLL mode_shift should be 10");
        assert_eq!(pll.lock_shift, 15);
    }

    #[test]
    fn test_all_pll_common_attributes() {
        // 验证所有 PLL 的通用属性
        for (idx, pll) in RK3588_PLL_CLOCKS.iter().enumerate() {
            // 所有 PLL 的类型应该是 RK3588
            assert_eq!(
                pll.pll_type,
                RockchipPllType::Rk3588,
                "PLL[{}] type should be RK3588",
                idx
            );

            // 所有 PLL 的锁定位都应该是 15
            assert_eq!(pll.lock_shift, 15, "PLL[{}] lock_shift should be 15", idx);

            // 所有 PLL 的标志位都应该是 0
            assert_eq!(pll.pll_flags, 0, "PLL[{}] flags should be 0", idx);

            // 所有 PLL 的模式掩码都应该是 0
            assert_eq!(pll.mode_mask, 0, "PLL[{}] mode_mask should be 0", idx);

            // 所有 PLL 应该使用相同的频率表
            assert_eq!(
                pll.rate_table.len(),
                17,
                "PLL[{}] rate_table should have 17 entries",
                idx
            );
        }
    }

    #[test]
    fn test_pll_rate_table_entries() {
        // 验证频率表中每个条目的参数
        let table = PLL_RATE_TABLE;

        // 条目 0: 1.5GHz
        let entry = &table[0];
        assert_eq!(entry.rate, 1_500_000_000);
        match entry.params {
            PllRateParams::Rk3588 { p, m, s, k } => {
                assert_eq!((p, m, s, k), (2, 250, 1, 0));
            }
            _ => panic!("Expected Rk3588 params"),
        }

        // 条目 1: 1.2GHz
        let entry = &table[1];
        assert_eq!(entry.rate, 1_200_000_000);
        match entry.params {
            PllRateParams::Rk3588 { p, m, s, k } => {
                assert_eq!((p, m, s, k), (2, 200, 1, 0));
            }
            _ => panic!("Expected Rk3588 params"),
        }

        // 条目 2: 1.188GHz (GPLL 默认)
        let entry = &table[2];
        assert_eq!(entry.rate, 1_188_000_000);
        match entry.params {
            PllRateParams::Rk3588 { p, m, s, k } => {
                assert_eq!((p, m, s, k), (2, 198, 1, 0));
            }
            _ => panic!("Expected Rk3588 params"),
        }

        // 条目 9: 786.432MHz (小数分频示例)
        let entry = &table[9];
        assert_eq!(entry.rate, 786_432_000);
        match entry.params {
            PllRateParams::Rk3588 { p, m, s, k } => {
                assert_eq!((p, m, s, k), (2, 262, 2, 9437));
            }
            _ => panic!("Expected Rk3588 params"),
        }

        // 最后一条: 100MHz
        let entry = &table[16];
        assert_eq!(entry.rate, 100_000_000);
        match entry.params {
            PllRateParams::Rk3588 { p, m, s, k } => {
                assert_eq!((p, m, s, k), (3, 400, 5, 0));
            }
            _ => panic!("Expected Rk3588 params"),
        }
    }

    #[test]
    fn test_pll_config_complete_validation() {
        // 完整验证: 对比 C 代码 clk_rk3588.c:46-67 的所有配置

        // B0PLL: PLL_B0PLL = 1
        let pll = get_pll(PllId::B0PLL);
        assert_eq!(pll.con_offset, b0_pll_con(0));
        assert_eq!(pll.mode_offset, RK3588_B0_PLL_MODE_CON);
        assert_eq!((pll.mode_shift, pll.lock_shift, pll.pll_flags), (0, 15, 0));

        // B1PLL: PLL_B1PLL = 2
        let pll = get_pll(PllId::B1PLL);
        assert_eq!(pll.con_offset, b1_pll_con(8));
        assert_eq!(pll.mode_offset, RK3588_B1_PLL_MODE_CON);
        assert_eq!((pll.mode_shift, pll.lock_shift, pll.pll_flags), (0, 15, 0));

        // LPLL: PLL_LPLL = 3
        let pll = get_pll(PllId::LPLL);
        assert_eq!(pll.con_offset, lpll_con(16));
        assert_eq!(pll.mode_offset, RK3588_LPLL_MODE_CON);
        assert_eq!((pll.mode_shift, pll.lock_shift, pll.pll_flags), (0, 15, 0));

        // V0PLL: PLL_V0PLL = 4
        let pll = get_pll(PllId::V0PLL);
        assert_eq!(pll.con_offset, pll_con(88));
        assert_eq!(pll.mode_offset, RK3588_MODE_CON0);
        assert_eq!((pll.mode_shift, pll.lock_shift, pll.pll_flags), (4, 15, 0));

        // AUPLL: PLL_AUPLL = 5
        let pll = get_pll(PllId::AUPLL);
        assert_eq!(pll.con_offset, pll_con(96));
        assert_eq!(pll.mode_offset, RK3588_MODE_CON0);
        assert_eq!((pll.mode_shift, pll.lock_shift, pll.pll_flags), (6, 15, 0));

        // CPLL: PLL_CPLL = 6
        let pll = get_pll(PllId::CPLL);
        assert_eq!(pll.con_offset, pll_con(104));
        assert_eq!(pll.mode_offset, RK3588_MODE_CON0);
        assert_eq!((pll.mode_shift, pll.lock_shift, pll.pll_flags), (8, 15, 0));

        // GPLL: PLL_GPLL = 7
        let pll = get_pll(PllId::GPLL);
        assert_eq!(pll.con_offset, pll_con(112));
        assert_eq!(pll.mode_offset, RK3588_MODE_CON0);
        assert_eq!((pll.mode_shift, pll.lock_shift, pll.pll_flags), (2, 15, 0));

        // NPLL: PLL_NPLL = 8
        let pll = get_pll(PllId::NPLL);
        assert_eq!(pll.con_offset, pll_con(120));
        assert_eq!(pll.mode_offset, RK3588_MODE_CON0);
        assert_eq!((pll.mode_shift, pll.lock_shift, pll.pll_flags), (0, 15, 0));

        // PPLL: PLL_PPLL = 9
        let pll = get_pll(PllId::PPLL);
        assert_eq!(pll.con_offset, pmu_pll_con(128));
        assert_eq!(pll.mode_offset, RK3588_MODE_CON0);
        assert_eq!((pll.mode_shift, pll.lock_shift, pll.pll_flags), (10, 15, 0));
    }

    #[test]
    fn test_pll_id_to_clk_id_conversion() {
        // 测试 PllId -> ClkId 转换
        let clk_id: ClkId = PllId::GPLL.into();
        assert_eq!(
            clk_id.value(),
            7,
            "GPLL should convert to ClkId with value 7"
        );

        let clk_id: ClkId = PllId::CPLL.into();
        assert_eq!(
            clk_id.value(),
            6,
            "CPLL should convert to ClkId with value 6"
        );

        let clk_id: ClkId = PllId::PPLL.into();
        assert_eq!(
            clk_id.value(),
            9,
            "PPLL should convert to ClkId with value 9"
        );
    }

    #[test]
    fn test_clk_id_to_pll_id_conversion() {
        // 测试 ClkId -> PllId 转换 (成功情况)
        let clk_id = ClkId::new(7);
        let pll_id = PllId::try_from(clk_id);
        assert!(pll_id.is_ok(), "ClkId(7) should convert to PllId::GPLL");
        assert_eq!(pll_id.unwrap(), PllId::GPLL);

        let clk_id = ClkId::new(6);
        let pll_id = PllId::try_from(clk_id);
        assert!(pll_id.is_ok(), "ClkId(6) should convert to PllId::CPLL");
        assert_eq!(pll_id.unwrap(), PllId::CPLL);

        let clk_id = ClkId::new(9);
        let pll_id = PllId::try_from(clk_id);
        assert!(pll_id.is_ok(), "ClkId(9) should convert to PllId::PPLL");
        assert_eq!(pll_id.unwrap(), PllId::PPLL);
    }

    #[test]
    fn test_clk_id_to_pll_id_invalid() {
        // 测试无效的 ClkId -> PllId 转换
        let clk_id = ClkId::new(100); // 无效的 PLL ID
        let result = PllId::try_from(clk_id);
        assert!(
            result.is_err(),
            "Invalid ClkId should fail to convert to PllId"
        );
        assert_eq!(result.unwrap_err(), "Invalid PLL clock ID");
    }

    #[test]
    fn test_pll_clk_id_round_trip() {
        // 测试双向转换的一致性
        let original_pll = PllId::NPLL;
        let clk_id: ClkId = original_pll.into();
        let converted_pll = PllId::try_from(clk_id).unwrap();
        assert_eq!(
            original_pll, converted_pll,
            "Round-trip conversion should preserve PllId"
        );

        let original_pll = PllId::B0PLL;
        let clk_id: ClkId = original_pll.into();
        let converted_pll = PllId::try_from(clk_id).unwrap();
        assert_eq!(
            original_pll, converted_pll,
            "Round-trip conversion should preserve PllId"
        );
    }
}
