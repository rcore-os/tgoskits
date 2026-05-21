# usb@fc400000 依赖关系图

## 概述

本文档详细描述了 OrangePi 5 Plus (RK3588) 上 `usb@fc400000` 节点的完整依赖关系。该节点对应 **USBDRD3_1** 控制器，即第二个 USB3.0 Dual-Role Device 控制器。

### 节点基本信息

- **节点路径**: `/usbdrd3_1/usb@fc400000`
- **物理地址**: `0xfc400000`
- **寄存器大小**: `0x400000` (4MB)
- **兼容字符串**: `snps,dwc3`
- **工作模式**: Host (主机模式)
- **支持速度**: USB 3.0 SuperSpeed, USB 2.0 HighSpeed
- **中断**: IRQ 221 (0xdd)

### 整体架构

`usb@fc400000` 是一个 DWC3 (DesignWare USB3) 控制器，支持 USB 3.0 和 USB 2.0。它依赖多个硬件子系统：

1. **物理层 (PHY)**: USB2 PHY 和 USB3/DP PHY
2. **电源管理**: Power Domain 和 VBUS 电源
3. **时钟和复位**: CRU (Clock and Reset Unit)
4. **系统控制**: GRF (General Register Files)
5. **GPIO 控制**: VBUS 使能信号

## Mermaid 依赖关系流程图

```mermaid
graph TD
    %% 第一层：USB 主控制器
    A[usb@fc400000<br/>DWC3 控制器<br/>0xfc400000<br/>phandle: 0x484]

    %% 第二层：直接依赖
    B[power-controller@fd8d8000<br/>电源域<br/>phandle: 0x61]
    C[CRU@fd7c0000<br/>时钟和复位<br/>phandle: 0x02]
    D[u2phy1_otg<br/>USB2 PHY<br/>phandle: 0x1aa]
    E[usbdp_phy1_u3<br/>USB3 PHY<br/>phandle: 0x1ab]
    F[vcc5v0-host<br/>VBUS 电源<br/>phandle: 0x76]
    G[usb_grf<br/>USB GRF<br/>phandle: 0x75]

    %% USB 主控制器依赖
    A -->|power-domains| B
    A -->|resets| C
    A -->|phys[0]| D
    A -->|phys[1]| E
    A -.->|power-supply| F
    A -.->|GRF| G

    %% 第三层：PHY 依赖
    H[u2phy1@fd5d4000<br/>USB2 PHY 控制器<br/>phandle: 0x1ce]
    I[usbdp_phy1@fed90000<br/>USB3/DP PHY 控制器<br/>phandle: 0x495]
    J[GPIO4@fec40000<br/>GPIO 控制器<br/>phandle: 0x10e]
    K[vcc5v0_sys<br/>系统电源<br/>phandle: 0x1e8]
    L[pinctrl-usb<br/>引脚控制<br/>phandle: 0x1ea]

    D -->|parent| H
    E -->|parent| I
    F -->|gpio| J
    F -->|vin-supply| K
    F -->|pinctrl-0| L

    %% USB2 PHY 依赖
    H -->|clocks| C
    H -->|resets| C
    H -->|usbctrl-grf| G

    %% USB3 PHY 依赖
    M[usb2phy_grf@fd5d4000<br/>USB2 PHY GRF<br/>phandle: 0x1cc]
    N[usbdpphy_grf@fd5cc000<br/>USBDP PHY GRF<br/>phandle: 0x1cd]
    O[vo_grf@fd5a6000<br/>VO GRF<br/>phandle: 0xfc]

    I -->|clocks| C
    I -->|resets| C
    I -->|u2phy-grf| M
    I -->|usb-grf| G
    I -->|usbdpphy-grf| N
    I -->|vo-grf| O

    %% GPIO 控制器依赖
    J -->|clocks| C
    J -->|resets| C

    %% 第四层：GRF 和基础框架
    P[PHP_GRF@fd5b0000<br/>PHP GRF<br/>phandle: 0x77]
    Q[syscon<br/>系统控制器框架]

    C -->|grf| P
    G -.->|框架| Q
    M -.->|框架| Q
    N -.->|框架| Q
    O -.->|框架| Q
    P -.->|框架| Q

    %% 样式
    classDef mainNode fill:#e1f5ff,stroke:#01579b,stroke-width:3px
    classDef phyNode fill:#fff3e0,stroke:#e65100,stroke-width:2px
    classDef sysNode fill:#f3e5f5,stroke:#4a148c,stroke-width:2px
    classDef grfNode fill:#e8f5e9,stroke:#1b5e20,stroke-width:2px

    class A mainNode
    class D,E,H,I phyNode
    class B,C,F,J,K,L sysNode
    class G,M,N,O,P grfNode
```

## 详细节点表格

### 第一层：USB 主控制器

| 节点名称 | 物理地址 | phandle | 驱动文件 | Compatible | 功能描述 |
|---------|---------|--------|---------|------------|----------|
| **usb@fc400000** | 0xfc400000 | 0x484 | `drivers/usb/host/xhci-dwc3.c` | `snps,dwc3` | DWC3 USB3 主控制器，支持 USB 3.0/2.0，工作在 Host 模式 |

**关键 DTS 属性**:
```dts
usb@fc400000 {
    compatible = "snps,dwc3";
    reg = <0x00 0xfc400000 0x00 0x400000>;
    interrupts = <0x00 0xdd 0x04>;
    power-domains = <0x61 0x1f>;
    resets = <0x02 0x2a7>;
    reset-names = "usb3-otg";
    dr_mode = "host";
    phys = <0x1aa 0x1ab>;
    phy-names = "usb2-phy\0usb3-phy";
    phy_type = "utmi_wide";
    snps,dis_enblslpm_quirk;
    snps,dis-u1-entry-quirk;
    snps,dis-u2-entry-quirk;
    snps,dis-u2-freeclk-exists-quirk;
    snps,dis-del-phy-power-chg-quirk;
    snps,dis-tx-ipgap-linecheck-quirk;
    snps,parkmode-disable-hs-quirk;
    snps,parkmode-disable-ss-quirk;
}
```

### 第二层：PHY 和电源管理

| 节点名称 | 物理地址 | phandle | 驱动文件 | Compatible | 功能描述 |
|---------|---------|--------|---------|------------|----------|
| **u2phy1** (usb2-phy@4000) | 0xfd5d4000 | 0x1ce | `drivers/phy/phy-rockchip-naneng-usb2.c` | `rockchip,rk3588-usb2phy` | USB2.0 PHY 控制器，支持 480Mbps |
| **usbdp_phy1** (phy@fed90000) | 0xfed90000 | 0x495 | `drivers/phy/phy-rockchip-usbdp.c` | `rockchip,rk3588-usbdp-phy` | USB3.0/DP Combo PHY，支持 5Gbps |
| **vcc5v0-host** | - | 0x76 | `drivers/power/regulator/fixed.c` | `regulator-fixed` | 5V VBUS 电源调节器 |

**USB2 PHY 详细属性**:
```dts
syscon@fd5d4000 {
    compatible = "rockchip,rk3588-usb2phy-grf\0syscon\0simple-mfd";
    reg = <0x00 0xfd5d4000 0x00 0x4000>;

    usb2-phy@4000 {
        compatible = "rockchip,rk3588-usb2phy";
        reg = <0x4000 0x10>;
        interrupts = <0x00 0x18a 0x04>;
        resets = <0x02 0xc0048 0x02 0x489>;  // phy, apb
        reset-names = "phy\0apb";
        clocks = <0x02 0x2b5>;  // phyclk
        clock-names = "phyclk";
        clock-output-names = "usb480m_phy1";
        #clock-cells = <0x00>;
        rockchip,usbctrl-grf = <0x75>;  // USB GRF

        otg-port {
            #phy-cells = <0x00>;
            phy-supply = <0x76>;  // vcc5v0-host
            phandle = <0x1aa>;
        };
    }
}
```

**USB3 PHY 详细属性**:
```dts
phy@fed90000 {
    compatible = "rockchip,rk3588-usbdp-phy";
    reg = <0x00 0xfed90000 0x00 0x10000>;

    // GRF 引用
    rockchip,u2phy-grf = <0x1cc>;  // USB2 PHY GRF
    rockchip,usb-grf = <0x75>;      // USB GRF
    rockchip,usbdpphy-grf = <0x1cd>; // USBDP PHY GRF
    rockchip,vo-grf = <0xfc>;       // VO GRF

    // 时钟引用
    clocks = <0x02 0x2b6  // refclk
             0x02 0x280  // immortal
             0x02 0x26a  // pclk
             0x1ce>;     // utmi
    clock-names = "refclk\0immortal\0pclk\0utmi";

    // 复位引用
    resets = <0x02 0x2f   // init
             0x02 0x30   // cmn
             0x02 0x31   // lane
             0x02 0x32   // pcs_apb
             0x02 0x484>; // pma_apb
    reset-names = "init\0cmn\0lane\0pcs_apb\0pma_apb";

    // DP lane 配置
    rockchip,dp-lane-mux = <0x02 0x03>;  // 使用 lane 2,3

    u3-port {
        #phy-cells = <0x00>;
        phandle = <0x1ab>;
    };
}
```

### 第三层：系统控制寄存器

| 节点名称 | 物理地址 | phandle | 驱动文件 | Compatible | 功能描述 |
|---------|---------|--------|---------|------------|----------|
| **usb_grf** | 0xfd5ac000 | 0x75 | `drivers/core/syscon-uclass.c` | `rockchip,rk3588-usb-grf\0syscon` | USB 控制寄存器，包含 USB3OTG 配置 |
| **usb2phy_grf** | 0xfd5d4000 | 0x1cc | `drivers/core/syscon-uclass.c` | `rockchip,rk3588-usb2phy-grf\0syscon` | USB2 PHY 寄存器 |
| **usbdpphy_grf** | 0xfd5cc000 | 0x1cd | `drivers/core/syscon-uclass.c` | `rockchip,rk3588-usbdpphy-grf\0syscon` | USBDP PHY 寄存器 |
| **vo_grf** | 0xfd5a6000 | 0xfc | `drivers/core/syscon-uclass.c` | `rockchip,rk3588-vo-grf\0syscon` | 视频/显示输出 GRF，用于 DP lane 选择 |
| **php_grf** | 0xfd5b0000 | 0x77 | `drivers/core/syscon-uclass.c` | `rockchip,rk3588-php-grf\0syscon` | PHP (Power/Performance/Hub) GRF |
| **gpio4** | 0xfec40000 | 0x10e | `drivers/gpio/rk_gpio.c` | `rockchip,gpio-bank` | GPIO4 控制器，PB0 用于 VBUS 使能 |

**GPIO4 详细属性**:
```dts
gpio4@fec40000 {
    compatible = "rockchip,gpio-bank";
    reg = <0x00 0xfec40000 0x00 0x100>;
    interrupts = <0x00 0xa3 0x04>;
    clocks = <0x02 0x81 0x02 0x82>;  // pclk, clk_gates
    gpio-controller;
    #gpio-cells = <0x02>;
    interrupt-controller;
    #interrupt-cells = <0x02>;
    phandle = <0x10e>;
}
```

### 第四层：基础框架

| 节点名称 | 物理地址 | phandle | 驱动文件 | Compatible | 功能描述 |
|---------|---------|--------|---------|------------|----------|
| **cru** | 0xfd7c0000 | 0x02 | `drivers/clk/rockchip/clk_rk3588.c` | `rockchip,rk3588-cru` | 时钟和复位单元 |
| **power-controller** | 0xfd8d8000 | 0x61 | `drivers/power/domain/rockchip/pm-domain.c` | `rockchip,rk3588-power-controller` | 电源域控制器 |
| **syscon** 框架 | - | - | `drivers/core/syscon-uclass.c` | `syscon` | 系统控制器基础框架 |

**CRU 详细属性**:
```dts
cru: clock-controller@fd7c0000 {
    compatible = "rockchip,rk3588-cru";
    reg = <0x00 0xfd7c0000 0x00 0x2000>;
    rockchip,grf = <0x77>;  // PHP GRF
    #clock-cells = <0x01>;
    #reset-cells = <0x01>;
    phandle = <0x02>;
}
```

## 驱动详细说明

### 1. DWC3 USB3 主控制器驱动

**驱动文件**: `drivers/usb/host/xhci-dwc3.c`

**Compatible 字符串**: `snps,dwc3`

**主要功能**:
- DWC3 控制器的核心驱动
- 支持 USB 3.0 SuperSpeed (5Gbps) 和 USB 2.0 HighSpeed (480Mbps)
- 实现 xHCI (eXtensible Host Controller Interface) 规范
- 支持主机模式和设备模式

**关键代码片段**:
```c
/* DWC3 控制器初始化 */
static int dwc3_init(struct dwc3 *dwc)
{
    /* 获取时钟 */
    ret = clk_get_by_index(dev, 0, &dwc->clk);
    if (ret)
        return ret;

    /* 使能时钟 */
    ret = clk_enable(&dwc->clk);
    if (ret)
        return ret;

    /* 获取复位信号 */
    ret = reset_get_by_index(dev, 0, &dwc->rsts);
    if (ret)
        return ret;

    /* 复位解除断言 */
    ret = reset_deassert(&dwc->rsts);
    if (ret)
        return ret;

    /* 初始化 DWC3 核心 */
    dwc3_core_init(dwc);

    return 0;
}
```

**重要寄存器地址**:
- **GBUSBAR**: 0xc100 - xHCI 寄存器基地址
- **GCTL**: 0xc110 - 全局控制寄存器
  - Bit 12: PRTCAPDIR (端口能力方向)
    - 0 = Device
    - 1 = Host
    - 2 = OTG
- **GUSB2PHYCFG**: 0xc200 - USB2 PHY 配置寄存器
  - Bit 6: SUSPHY (Suspend PHY)
  - Bit 8-9: PHYIF (UTMI 接口宽度)
  - Bit 10-19: USBTRDTIM (UTMI 转发延迟)
- **GUSB3PIPECTL**: 0xc230 - USB3 PHY 管道控制寄存器
  - Bit 15: PIPE_ENABLE
  - Bit 12: PHY_DISABLE
  - Bit 8: U3_PORT_DISABLE

### 2. USB2 PHY 驱动 (Naneng)

**驱动文件**: `drivers/phy/phy-rockchip-naneng-usb2.c`

**Compatible 字符串**:
- `rockchip,rk3588-usb2phy`
- `rockchip,rv1126-usb2phy`

**主要功能**:
- Rockchip Naneng USB2.0 PHY 驱动
- 支持 USB 2.0 HighSpeed (480Mbps)
- 支持 OTG 模式检测
- 提供 480MHz 时钟输出
- 支持 VBUS 检测和 ID 检测

**关键代码片段**:
```c
/* USB2 PHY 初始化 */
static int rockchip_usb2phy_init(struct phy *phy)
{
    struct rockchip_usb2phy *priv = dev_get_priv(phy->dev);

    /* 等待 UTMI 时钟稳定 */
    udelay(2000);

    /* 退出 IDDQ 模式 */
    reg = readl(priv->mmio + 0x0008);
    reg &= ~BIT(29);  // 清除 IDDQ 位
    writel(reg, priv->mmio + 0x0008);

    /* 配置 HS 发送器预加重 */
    reg = readl(priv->mmio + 0x0030);
    reg |= BIT(3);  // 2x 预加重
    writel(reg, priv->mmio + 0x0030);

    /* 禁用 PHY 挂起 */
    reg = readl(grf_base + USB2PHY_GRF_CON(0));
    reg &= ~BIT(0);  // 清除 PORT0_SUSPEND
    writel(reg, grf_base + USB2PHY_GRF_CON(0));

    return 0;
}
```

**重要寄存器地址**:
- **CLK_CONTROL**: 0x0000 - 时钟控制寄存器
- **CON0**: 0x0008 - 配置寄存器 0
  - Bit 29: IDDQ (低功耗模式)
  - Bit 24: UTMI_OTA_DISABLE
  - Bit 18: OTG_DISABLE
- **BCDRIVE**: 0x001c - 总线充电驱动
- **HPTXFSLS**: 0x0030 - 高速发送器 FSL 调节
  - Bit 3-4: PREEMPH (预加重强度)
  - Bit 0-2: FSL_TUNE (全速电平调节)

### 3. USB3/DP Combo PHY 驱动

**驱动文件**: `drivers/phy/phy-rockchip-usbdp.c`

**Compatible 字符串**: `rockchip,rk3588-usbdp-phy`

**主要功能**:
- USB3.0 和 DisplayPort Combo PHY
- 支持 USB 3.0 SuperSpeed (5Gbps)
- 支持 DP 1.4 (HBR3: 8.1Gbps)
- 支持 4 个 lane 的灵活配置
- 支持 USB+DP 组合模式

**关键代码片段**:
```c
/* USBDP PHY 初始化 */
static int rockchip_usbdp_phy_init(struct phy *phy)
{
    struct rockchip_udphy *udphy = dev_get_priv(phy->dev);

    /* 启用 RX LFPS (Low Frequency Periodic Signaling) */
    reg = readl(udphy->usbdpphy_grf + 0x0004);
    reg |= BIT(14);  // RX_LFPS_EN
    writel(reg, udphy->usbdpphy_grf + 0x0004);

    /* 退出低功耗模式 */
    reg = readl(udphy->usbdpphy_grf + 0x0004);
    reg |= BIT(13);  // LOW_PWRN
    writel(reg, udphy->usbdpphy_grf + 0x0004);

    /* 配置 USB3OTG 寄存器 */
    reg = readl(udphy->usb_grf + 0x0034);
    reg &= ~BIT(15);  // PIPE_ENABLE = 1
    reg |= BIT(12);   // PHY_DISABLE = 0
    reg &= ~BIT(8);   // U3_PORT_DISABLE = 0
    writel(reg, udphy->usb_grf + 0x0034);

    /* 配置 lane mux */
    reg = readl(udphy->vo_grf + 0x0008);
    reg &= ~(0xFF << 16);  // 清除 lane 值
    reg |= (0x40 << 16);   // 设置 lane 2/3 为 DP
    writel(reg, udphy->vo_grf + 0x0008);

    /* 等待 PLL 锁定 */
    ret = readl_poll_timeout(udphy->mmio + 0x80a0, reg,
                             reg & BIT(3), 100, 50000);

    return 0;
}
```

**重要寄存器地址**:
- **USBDPPHY_LOW_PWRN** (usbdpphy_grf + 0x0004): 低功耗控制
  - Bit 14: RX_LFPS (接收 LFPS 使能)
  - Bit 13: LOW_PWRN (低功耗模式)
- **USB3OTG1_CFG** (usb_grf + 0x0034): USB3 OTG1 配置
  - Bit 15: PIPE_ENABLE (管道使能)
  - Bit 12: PHY_DISABLE (PHY 禁用)
  - Bit 8: U3_PORT_DISABLE (U3 端口禁用)
- **VO0_CON_DP_LANE_MUX** (vo_grf + 0x0008): DP lane 复用选择
  - Bit 16-23: Lane mux 值

### 4. CRU (时钟和复位单元) 驱动

**驱动文件**: `drivers/clk/rockchip/clk_rk3588.c`

**Compatible 字符串**: `rockchip,rk3588-cru`

**主要功能**:
- 为所有外设提供时钟
- 控制复位信号
- 支持 PLL 配置
- 支持时钟门控和分频

**USB 相关时钟**:
- **CLK_USB2PHY_HDPTXRXPHY_REF** (0x2b5): USB2 PHY 参考时钟
- **CLK_USBDPPHY_MIPIDCPPHY_REF** (0x2b6): USBDP PHY 参考时钟
- **CLK_USBDP_PHY1_IMMORTAL** (0x280): USBDP PHY 永久时钟
- **PCLK_USBDPPHY1** (0x26a): USBDP PHY APB 时钟
- **CLK_REF_PIPE_PHY1** (utmi): PIPE 参考时钟
- **PCLK_PCIE_COMBO_PIPE_PHY1**: Combo PHY 时钟
- **PCLK_GPIO4** (0x81): GPIO4 APB 时钟

**复位信号**:
- **USB3OTG** (0x2a7): USB3 OTG 复位
- **USBDP_PHY1_INIT** (0x2f): PHY 初始化复位
- **USBDP_PHY1_CMN** (0x30): 公共模块复位
- **USBDP_PHY1_LANE** (0x31): Lane 复位
- **USBDP_PHY1_PCS_APB** (0x32): PCS APB 复位
- **USBDP_PHY1_PMA_APB** (0x484): PMA APB 复位
- **USB2PHY1_PHY** (0xc0048): USB2 PHY 复位
- **USB2PHY1_APB** (0x489): USB2 PHY APB 复位

### 5. Syscon (系统控制器) 驱动

**驱动文件**: `drivers/core/syscon-uclass.c`

**Compatible 字符串**: `syscon`

**主要功能**:
- 提供对系统控制寄存器的访问
- 支持 regmap 接口
- 为各个子系统提供配置接口

**关键代码片段**:
```c
/* Syscon 初始化 */
static int syscon_probe(struct udevice *dev)
{
    struct syscon_uc_info *priv = dev_get_priv(dev);

    /* 映射寄存器 */
    priv->regs = dev_read_addr_ptr(dev);
    if (!priv->regs)
        return -EINVAL;

    /* 创建 regmap */
    regmap_init_mem(dev_ofnode(dev), &priv->regmap);

    return 0;
}

/* 读取寄存器 */
static int syscon_read(struct udevice *dev, unsigned int offset,
                       unsigned int *val)
{
    struct syscon_uc_info *priv = dev_get_priv(dev);

    *val = readl(priv->regs + offset);

    return 0;
}

/* 写入寄存器 */
static int syscon_write(struct udevice *dev, unsigned int offset,
                        unsigned int val)
{
    struct syscon_uc_info *priv = dev_get_priv(dev);

    writel(val, priv->regs + offset);

    return 0;
}
```

## 初始化顺序

正确的硬件初始化顺序对于 USB 控制器正常工作至关重要。以下是从 U-Boot 源码中提取的初始化顺序：

### 1. 系统基础初始化

```
1.1 CRU 初始化
    └── 启用基础时钟和 PLL

1.2 Power Domain 初始化
    └── 启用 USB 和 PHP 电源域
        ├── USB_PD (0x1f): usb@fc400000
        └── PHP_PD (0x20): USBDP PHY
```

### 2. GPIO 和引脚配置

```
2.1 pinctrl 初始化
    └── 配置 GPIO4_PB0 为 VBUS 使能

2.2 VBUS 电源使能
    └── 设置 GPIO4_PB0 输出高电平
        └── 启用 vcc5v0-host 电源
```

### 3. PHY 硬件初始化

```
3.1 复位序列
    ├── Assert 所有 PHY 复位
    ├── 启用 PHY 时钟
    ├── 等待时钟稳定
    └── Deassert PHY 复位

3.2 USB2 PHY 初始化
    ├── 退出 IDDQ 模式
    ├── 配置 HS 发送器
    ├── 等待 UTMI 时钟稳定 (2ms)
    └── 禁用 PHY 挂起

3.3 USB3/DP PHY 初始化
    ├── 配置低功耗控制
    ├── 配置 USB3OTG 寄存器
    ├── 配置 lane mux
    ├── 等待 PLL 锁定
    └── 启用 pipe
```

### 4. DWC3 控制器初始化

```
4.1 控制器软复位
    ├── Device soft reset
    └── Core soft reset

4.2 全局配置
    ├── 配置 GCTL (主机模式)
    ├── 配置 GUSB2PHYCFG (UTMI 16-bit)
    ├── 配置 GUSB3PIPECTL (禁用 SUSPHY)
    └── 配置事件缓冲区

4.3 xHCI 初始化
    ├── HCRST (xHCI 主机控制器复位)
    ├── 配置 DCBAA (设备上下文数组)
    ├── 配置 CRCR (命令环控制)
    ├── 配置 ERST (事件环)
    └── 启用主控制器
```

### 5. 端口配置

```
5.1 端口复位
    └── 启动端口复位序列

5.2 等待设备连接
    └── 轮询端口状态
        ├── 当前连接状态 (CCS)
        ├── 端口使能状态
        ├── 端口链路状态 (PLS)
        └── 端口速度
```

### 关键时序要求

1. **UTMI 时钟稳定时间**: 2ms (USB2 PHY)
2. **PLL 锁定时间**: 50ms (USB3 PHY)
3. **端口复位持续时间**: 50ms
4. **设备检测超时**: 100ms

### 寄存器配置顺序

```c
/* 1. 电源域 */
power-domains = <0x61 0x1f>;  // USB_PD

/* 2. PHY 时钟 */
clocks = <0x02 0x2b5>;  // CLK_USB2PHY_HDPTXRXPHY_REF

/* 3. PHY 复位 */
resets = <0x02 0xc0048>;  // USB2PHY_PHY

/* 4. PHY 配置 */
rockchip,usbctrl-grf = <0x75>;

/* 5. DWC3 配置 */
reg = <0xfc400000 0x400000>;
dr_mode = "host";
phys = <0x1aa 0x1ab>;
```

## 参考资源

### U-Boot 源码路径

- **DWC3 控制器**: `/home/zhourui/opensource/proj_usb/u-boot-orangepi/drivers/usb/host/xhci-dwc3.c`
- **USB2 PHY**: `/home/zhourui/opensource/proj_usb/u-boot-orangepi/drivers/phy/phy-rockchip-naneng-usb2.c`
- **USB3/DP PHY**: `/home/zhourui/opensource/proj_usb/u-boot-orangepi/drivers/phy/phy-rockchip-usbdp.c`
- **Combo PHY**: `/home/zhourui/opensource/proj_usb/u-boot-orangepi/drivers/phy/phy-rockchip-naneng-combphy.c`
- **CRU**: `/home/zhourui/opensource/proj_usb/u-boot-orangepi/drivers/clk/rockchip/clk_rk3588.c`
- **Syscon**: `/home/zhourui/opensource/proj_usb/u-boot-orangepi/drivers/core/syscon-uclass.c`

### 设备树文件

- **OrangePi 5 Plus**: `/home/zhourui/opensource/proj_usb/u-boot-orangepi/orangepi5plus.dts`
- **RK3588 基础**: `/home/zhourui/opensource/proj_usb/u-boot-orangepi/arch/arm/dts/rk3588.dtsi`

### 参考手册

- **RK3588 TRM**: `/home/zhourui/opensource/proj_usb/CrabUSB2/.spec-workflow/Rockchip_RK3588_TRM_V1.0-Part2.md`
- **DWC3 规格书**: Synopsys DWC3 USB3 Controller Databook

---

**文档版本**: 1.0
**生成日期**: 2025-01-08
**平台**: OrangePi 5 Plus (RK3588)
**内核**: U-Boot 2021.01
