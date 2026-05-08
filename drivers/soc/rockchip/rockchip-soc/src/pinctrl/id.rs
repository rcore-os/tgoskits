use core::fmt::{Debug, Display};

/// 全局引脚标识 (0-159)
///
/// RK3588 有 5 个 GPIO bank，每个 bank 32 个引脚，共 160 个引脚。
/// 引脚编号规则：pin_id = bank_id * 32 + pin_in_bank
///
/// # 示例
///
/// ```
/// use rockchip_soc::pinctrl::PinId;
///
/// // GPIO0_A0 = PinId 0
/// let pin0 = PinId::new(0).unwrap();
///
/// // GPIO1_A0 = PinId 32
/// let pin32 = PinId::new(32).unwrap();
///
/// // 从 bank 和 pin 创建
/// use rockchip_soc::pinctrl::BankId;
/// let pin = PinId::from_bank_pin(BankId::new(1).unwrap(), 0).unwrap();
/// assert_eq!(pin.raw(), 32);
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PinId(u32);

impl Debug for PinId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        Display::fmt(self, f)?;
        write!(f, "({})", self.raw())
    }
}

impl Display for PinId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let pin = self.pin_in_bank();
        let group = match pin / 8 {
            0 => 'A',
            1 => 'B',
            2 => 'C',
            3 => 'D',
            _ => '?',
        };
        write!(f, "GPIO{}-{group}{}", self.bank().raw(), pin % 8)
    }
}

impl PinId {
    /// 创建新的 PinId
    ///
    /// # 参数
    ///
    /// * `id` - 引脚编号 (0-159)
    ///
    /// # 返回
    ///
    /// 如果 id < 160，返回 `Some(PinId)`，否则返回 `None`
    pub const fn new(id: u32) -> Option<Self> {
        if id < 160 { Some(Self(id)) } else { None }
    }

    /// 获取原始引脚编号
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// 获取引脚所属的 bank (0-4)
    pub const fn bank(self) -> BankId {
        BankId(self.0 / 32)
    }

    /// 获取在 bank 内的引脚编号 (0-31)
    pub const fn pin_in_bank(self) -> u32 {
        self.0 % 32
    }

    /// 从 bank 和 pin_in_bank 创建 PinId
    ///
    /// # 参数
    ///
    /// * `bank` - GPIO bank 标识 (0-4)
    /// * `pin` - bank 内的引脚编号 (0-31)
    pub const fn from_bank_pin(bank: BankId, pin: u32) -> Option<Self> {
        if pin < 32 {
            Some(Self(bank.0 * 32 + pin))
        } else {
            None
        }
    }
}

/// GPIO bank 标识 (0-4)
///
/// RK3588 有 5 个 GPIO bank，每个 bank 包含 32 个引脚。
///
/// # 示例
///
/// ```
/// use rockchip_soc::pinctrl::BankId;
///
/// let bank0 = BankId::new(0).unwrap();
/// assert_eq!(bank0.raw(), 0);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BankId(u32);

impl BankId {
    /// 创建新的 BankId
    ///
    /// # 参数
    ///
    /// * `id` - bank 编号 (0-4)
    ///
    /// # 返回
    ///
    /// 如果 id < 5，返回 `Some(BankId)`，否则返回 `None`
    pub const fn new(id: u32) -> Option<Self> {
        if id < 5 { Some(Self(id)) } else { None }
    }

    /// 获取原始 bank 编号
    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl From<u32> for BankId {
    fn from(id: u32) -> Self {
        Self(id)
    }
}

pub const GPIO0_A0: PinId = PinId(0);
pub const GPIO0_A1: PinId = PinId(1);
pub const GPIO0_A2: PinId = PinId(2);
pub const GPIO0_A3: PinId = PinId(3);
pub const GPIO0_A4: PinId = PinId(4);
pub const GPIO0_A5: PinId = PinId(5);
pub const GPIO0_A6: PinId = PinId(6);
pub const GPIO0_A7: PinId = PinId(7);
pub const GPIO0_B0: PinId = PinId(8);
pub const GPIO0_B1: PinId = PinId(9);
pub const GPIO0_B2: PinId = PinId(10);
pub const GPIO0_B3: PinId = PinId(11);
pub const GPIO0_B4: PinId = PinId(12);
pub const GPIO0_B5: PinId = PinId(13);
pub const GPIO0_B6: PinId = PinId(14);
pub const GPIO0_B7: PinId = PinId(15);
pub const GPIO0_C0: PinId = PinId(16);
pub const GPIO0_C1: PinId = PinId(17);
pub const GPIO0_C2: PinId = PinId(18);
pub const GPIO0_C3: PinId = PinId(19);
pub const GPIO0_C4: PinId = PinId(20);
pub const GPIO0_C5: PinId = PinId(21);
pub const GPIO0_C6: PinId = PinId(22);
pub const GPIO0_C7: PinId = PinId(23);
pub const GPIO0_D0: PinId = PinId(24);
pub const GPIO0_D1: PinId = PinId(25);
pub const GPIO0_D2: PinId = PinId(26);
pub const GPIO0_D3: PinId = PinId(27);
pub const GPIO0_D4: PinId = PinId(28);
pub const GPIO0_D5: PinId = PinId(29);
pub const GPIO0_D6: PinId = PinId(30);
pub const GPIO0_D7: PinId = PinId(31);

pub const GPIO1_A0: PinId = PinId(32);
pub const GPIO1_A1: PinId = PinId(33);
pub const GPIO1_A2: PinId = PinId(34);
pub const GPIO1_A3: PinId = PinId(35);
pub const GPIO1_A4: PinId = PinId(36);
pub const GPIO1_A5: PinId = PinId(37);
pub const GPIO1_A6: PinId = PinId(38);
pub const GPIO1_A7: PinId = PinId(39);
pub const GPIO1_B0: PinId = PinId(40);
pub const GPIO1_B1: PinId = PinId(41);
pub const GPIO1_B2: PinId = PinId(42);
pub const GPIO1_B3: PinId = PinId(43);
pub const GPIO1_B4: PinId = PinId(44);
pub const GPIO1_B5: PinId = PinId(45);
pub const GPIO1_B6: PinId = PinId(46);
pub const GPIO1_B7: PinId = PinId(47);
pub const GPIO1_C0: PinId = PinId(48);
pub const GPIO1_C1: PinId = PinId(49);
pub const GPIO1_C2: PinId = PinId(50);
pub const GPIO1_C3: PinId = PinId(51);
pub const GPIO1_C4: PinId = PinId(52);
pub const GPIO1_C5: PinId = PinId(53);
pub const GPIO1_C6: PinId = PinId(54);
pub const GPIO1_C7: PinId = PinId(55);
pub const GPIO1_D0: PinId = PinId(56);
pub const GPIO1_D1: PinId = PinId(57);
pub const GPIO1_D2: PinId = PinId(58);
pub const GPIO1_D3: PinId = PinId(59);
pub const GPIO1_D4: PinId = PinId(60);
pub const GPIO1_D5: PinId = PinId(61);
pub const GPIO1_D6: PinId = PinId(62);
pub const GPIO1_D7: PinId = PinId(63);

pub const GPIO2_A0: PinId = PinId(64);
pub const GPIO2_A1: PinId = PinId(65);
pub const GPIO2_A2: PinId = PinId(66);
pub const GPIO2_A3: PinId = PinId(67);
pub const GPIO2_A4: PinId = PinId(68);
pub const GPIO2_A5: PinId = PinId(69);
pub const GPIO2_A6: PinId = PinId(70);
pub const GPIO2_A7: PinId = PinId(71);
pub const GPIO2_B0: PinId = PinId(72);
pub const GPIO2_B1: PinId = PinId(73);
pub const GPIO2_B2: PinId = PinId(74);
pub const GPIO2_B3: PinId = PinId(75);
pub const GPIO2_B4: PinId = PinId(76);
pub const GPIO2_B5: PinId = PinId(77);
pub const GPIO2_B6: PinId = PinId(78);
pub const GPIO2_B7: PinId = PinId(79);
pub const GPIO2_C0: PinId = PinId(80);
pub const GPIO2_C1: PinId = PinId(81);
pub const GPIO2_C2: PinId = PinId(82);
pub const GPIO2_C3: PinId = PinId(83);
pub const GPIO2_C4: PinId = PinId(84);
pub const GPIO2_C5: PinId = PinId(85);
pub const GPIO2_C6: PinId = PinId(86);
pub const GPIO2_C7: PinId = PinId(87);
pub const GPIO2_D0: PinId = PinId(88);
pub const GPIO2_D1: PinId = PinId(89);
pub const GPIO2_D2: PinId = PinId(90);
pub const GPIO2_D3: PinId = PinId(91);
pub const GPIO2_D4: PinId = PinId(92);
pub const GPIO2_D5: PinId = PinId(93);
pub const GPIO2_D6: PinId = PinId(94);
pub const GPIO2_D7: PinId = PinId(95);

pub const GPIO3_A0: PinId = PinId(96);
pub const GPIO3_A1: PinId = PinId(97);
pub const GPIO3_A2: PinId = PinId(98);
pub const GPIO3_A3: PinId = PinId(99);
pub const GPIO3_A4: PinId = PinId(100);
pub const GPIO3_A5: PinId = PinId(101);
pub const GPIO3_A6: PinId = PinId(102);
pub const GPIO3_A7: PinId = PinId(103);
pub const GPIO3_B0: PinId = PinId(104);
pub const GPIO3_B1: PinId = PinId(105);
pub const GPIO3_B2: PinId = PinId(106);
pub const GPIO3_B3: PinId = PinId(107);
pub const GPIO3_B4: PinId = PinId(108);
pub const GPIO3_B5: PinId = PinId(109);
pub const GPIO3_B6: PinId = PinId(110);
pub const GPIO3_B7: PinId = PinId(111);
pub const GPIO3_C0: PinId = PinId(112);
pub const GPIO3_C1: PinId = PinId(113);
pub const GPIO3_C2: PinId = PinId(114);
pub const GPIO3_C3: PinId = PinId(115);
pub const GPIO3_C4: PinId = PinId(116);
pub const GPIO3_C5: PinId = PinId(117);
pub const GPIO3_C6: PinId = PinId(118);
pub const GPIO3_C7: PinId = PinId(119);
pub const GPIO3_D0: PinId = PinId(120);
pub const GPIO3_D1: PinId = PinId(121);
pub const GPIO3_D2: PinId = PinId(122);
pub const GPIO3_D3: PinId = PinId(123);
pub const GPIO3_D4: PinId = PinId(124);
pub const GPIO3_D5: PinId = PinId(125);
pub const GPIO3_D6: PinId = PinId(126);
pub const GPIO3_D7: PinId = PinId(127);

pub const GPIO4_A0: PinId = PinId(128);
pub const GPIO4_A1: PinId = PinId(129);
pub const GPIO4_A2: PinId = PinId(130);
pub const GPIO4_A3: PinId = PinId(131);
pub const GPIO4_A4: PinId = PinId(132);
pub const GPIO4_A5: PinId = PinId(133);
pub const GPIO4_A6: PinId = PinId(134);
pub const GPIO4_A7: PinId = PinId(135);
pub const GPIO4_B0: PinId = PinId(136);
pub const GPIO4_B1: PinId = PinId(137);
pub const GPIO4_B2: PinId = PinId(138);
pub const GPIO4_B3: PinId = PinId(139);
pub const GPIO4_B4: PinId = PinId(140);
pub const GPIO4_B5: PinId = PinId(141);
pub const GPIO4_B6: PinId = PinId(142);
pub const GPIO4_B7: PinId = PinId(143);
pub const GPIO4_C0: PinId = PinId(144);
pub const GPIO4_C1: PinId = PinId(145);
pub const GPIO4_C2: PinId = PinId(146);
pub const GPIO4_C3: PinId = PinId(147);
pub const GPIO4_C4: PinId = PinId(148);
pub const GPIO4_C5: PinId = PinId(149);
pub const GPIO4_C6: PinId = PinId(150);
pub const GPIO4_C7: PinId = PinId(151);
pub const GPIO4_D0: PinId = PinId(152);
pub const GPIO4_D1: PinId = PinId(153);
pub const GPIO4_D2: PinId = PinId(154);
pub const GPIO4_D3: PinId = PinId(155);
pub const GPIO4_D4: PinId = PinId(156);
pub const GPIO4_D5: PinId = PinId(157);
pub const GPIO4_D6: PinId = PinId(158);
pub const GPIO4_D7: PinId = PinId(159);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gpio0_constants() {
        assert_eq!(GPIO0_A0.raw(), 0);
        assert_eq!(GPIO0_A7.raw(), 7);
        assert_eq!(GPIO0_B0.raw(), 8);
        assert_eq!(GPIO0_D7.raw(), 31);
    }

    #[test]
    fn test_gpio1_constants() {
        assert_eq!(GPIO1_A0.raw(), 32);
        assert_eq!(GPIO1_D7.raw(), 63);
    }

    #[test]
    fn test_gpio2_constants() {
        assert_eq!(GPIO2_A0.raw(), 64);
        assert_eq!(GPIO2_D7.raw(), 95);
    }

    #[test]
    fn test_gpio3_constants() {
        assert_eq!(GPIO3_A0.raw(), 96);
        assert_eq!(GPIO3_D7.raw(), 127);
    }

    #[test]
    fn test_gpio4_constants() {
        assert_eq!(GPIO4_A0.raw(), 128);
        assert_eq!(GPIO4_D7.raw(), 159);
    }

    #[test]
    fn test_all_banks() {
        // 测试每个 bank 的起始引脚
        assert_eq!(GPIO0_A0.raw(), 0);
        assert_eq!(GPIO1_A0.raw(), 32);
        assert_eq!(GPIO2_A0.raw(), 64);
        assert_eq!(GPIO3_A0.raw(), 96);
        assert_eq!(GPIO4_A0.raw(), 128);
    }

    #[test]
    fn test_pin_ranges() {
        // GPIO0: 0-31
        assert!(GPIO0_A0.raw() <= 31);
        assert!(GPIO0_D7.raw() <= 31);

        // GPIO1: 32-63
        assert!(GPIO1_A0.raw() >= 32 && GPIO1_A0.raw() <= 63);
        assert!(GPIO1_D7.raw() >= 32 && GPIO1_D7.raw() <= 63);

        // GPIO2: 64-95
        assert!(GPIO2_A0.raw() >= 64 && GPIO2_A0.raw() <= 95);
        assert!(GPIO2_D7.raw() >= 64 && GPIO2_D7.raw() <= 95);

        // GPIO3: 96-127
        assert!(GPIO3_A0.raw() >= 96 && GPIO3_A0.raw() <= 127);
        assert!(GPIO3_D7.raw() >= 96 && GPIO3_D7.raw() <= 127);

        // GPIO4: 128-159
        assert!(GPIO4_A0.raw() >= 128 && GPIO4_A0.raw() <= 159);
        assert!(GPIO4_D7.raw() >= 128 && GPIO4_D7.raw() <= 159);
    }
}
