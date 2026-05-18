# RK3588 USB3 设备发现流程完整分析

## 概述

本文档详细分析了在 RK3588 平台上执行 `usb start` 命令后，USB3 接口中的设备是如何被发现的。包括完整的驱动初始化顺序、设备枚举流程和硬件配置细节。

---

## 一、执行流程全景图

### 1.1 完整调用链

```
用户输入: usb start
    ↓
命令层解析
    ├─ 文件: cmd/usb.c:635
    ├─ 函数: do_usb()
    └─ 逻辑: 识别 "start" 子命令
    ↓
启动 USB 子系统
    ├─ 文件: cmd/usb.c:583
    ├─ 函数: do_usb_start()
    └─ 输出: "starting USB...\n"
    ↓
USB 子系统初始化
    ├─ 文件: drivers/usb/host/usb-uclass.c:242
    ├─ 函数: usb_init()
    ├─ 步骤 1: 控制器探测 (device_probe)
    │   ├─ DWC3 Glue 层初始化
    │   ├─ PHY 初始化
    │   ├─ DWC3 核心初始化
    │   └─ XHCI 控制器注册
    └─ 步骤 2: 设备扫描 (usb_scan_bus)
        ├─ Root Hub 创建
        ├─ 端口扫描
        ├─ 设备检测
        ├─ 设备枚举
        └─ Hub 递归扫描
    ↓
完成，输出设备信息
```

### 1.2 执行时序图

```
时间轴    命令层          USB子系统      控制器层         PHY层        设备层
  |         |                |             |              |           |
  |  usb start               |             |              |           |
  |----------> do_usb()      |             |              |           |
  |          -------------> do_usb_start()|              |           |
  |          ------------------------> usb_init()       |           |
  |                                     |              |           |
  | T1: 控制器探测阶段                  |              |           |
  |                                     device_probe()|           |
  |                                     ------------> dwc3_generic_probe
  |                                              |              |
  |                                              |    clk_enable_bulk()
  |                                              |    reset_assert()
  |                                              |----------> PHY初始化
  |                                              |    reset_deassert()
  |                                              |<-----------|
  |                                              |              |
  |                                              ------------> dwc3_init()
  |                                                     |         |
  |                                                     |----> dwc3_core_init()
  |                                                     |----> dwc3_core_init_mode()
  |                                              <------------|
  |                                              |
  |                                              ------------> xhci_register()
  |                                                     |
  |                                                     |----> xhci_lowlevel_init()
  |                                                     |         |
  |                                                     |----> xhci_reset()
  |                                                     |----> xhci_mem_init()
  |                                                     |----> xhci_run()
  |                                              <------------|
  |                                     <-------------|
  |                                     |
  | T2: 设备扫描阶段                    |
  |                                     usb_scan_bus()
  |                                     ------------> usb_scan_device()
  |                                              |
  |                                              |----> usb_alloc_device() [XHCI Enable Slot]
  |                                              |----> usb_setup_device()
  |                                              |         |
  |                                              |         |----> usb_prepare_device()
  |                                              |         |         |
  |                                              |         |         |---> 端口复位
  |                                              |         |         |---> usb_set_address()
  |                                              |         |         |---> 读取设备描述符
  |                                              |         |<---------|
  |                                              |         |
  |                                              |         |----> usb_select_config()
  |                                              |         |         |
  |                                              |         |         |---> 读取配置描述符
  |                                              |         |         |---> 解析接口/端点
  |                                              |         |         |---> usb_set_configuration()
  |                                              |         |<---------|
  |                                              |<---------|
  |                                              |
  |                                              |----> usb_hub_probe() [如果是Hub]
  |                                              |         |
  |                                              |         |---> usb_hub_configure()
  |                                              |         |---> 递归扫描子端口
  |                                              |         |
  |                                              |<---------|
  |                                     <-------------|
  |<------------------------------------|
  |
  V 完成
```

---

## 二、驱动初始化详解（自底向上）

### 2.1 硬件层：RK3588 USB3 控制器

**物理地址映射**：
- `usbdrd3_0`: 0xfc000000 (OTG 模式，支持 USB3.0)
- `usbdrd3_1`: 0xfc400000 (Host 模式，支持 USB3.0)
- `usbhost3_0`: 0xfcd00000 (专用 Host，仅 USB3.0)

**寄存器结构**：
```c
// DWC3 全局寄存器 (偏移 0xC100)
struct dwc3_regs {
    u32 g_ctl;           // 0xC100: 全局控制
    u32 g_sts;           // 0xC104: 全局状态
    u32 g_buscfg;        // 0xC108: 总线配置
    // ... 更多寄存器
};

// XHCI 主机控制器寄存器
struct xhci_hccr {
    u32 cr_capbase;      // 0x00: 能力基址
    u32 cr_hcsparams1;   // 0x04: 结构参数1
    u32 cr_hcsparams2;   // 0x08: 结构参数2
    // ... 更多寄存器
};

struct xhci_hcor {
    u32 usbcmd;          // 0x00: USB 命令
    u32 usbsts;          // 0x04: USB 状态
    u32 pagesize;        // 0x08: 页面大小
    // ... 更多寄存器
};
```

### 2.2 PHY 层：物理层初始化

**文件位置**：
- USB3 PHY: `drivers/phy/rockchip/phy-rockchip-inno-usb3.c`
- USB2 PHY: `drivers/phy/rockchip/phy-rockchip-naneng-usb2.c`
- USBDP Combo PHY: `drivers/phy/rockchip/phy-rockchip-usbdp.c`

**PHY 初始化调用链**：
```c
// drivers/usb/dwc3/dwc3-generic.c:74
rc = dwc3_setup_phy(dev, &priv->phys, &priv->num_phys);
    ↓
// drivers/usb/dwc3/core.c
generic_phy_get_bulk(dev, phys, num_phys)  // 获取 PHY 引用
    ↓
generic_phy_init(phys[i])                  // 初始化每个 PHY
    ↓
// drivers/phy/rockchip/xxx_phy.c
rockchip_usb3phy_init()                    // Rockchip PHY 初始化
    ├─ 时钟使能 (clk_enable)
    ├─ 复位控制 (reset_deassert)
    ├─ 寄存器配置 (GRF 寄存器)
    └─ PHY 上电 (power_on)
```

**RK3588 PHY 配置**（设备树）：

```dts
// USB2 PHY
u2phy1: usb2-phy@4000 {
    compatible = "rockchip,rk3588-usb2phy";
    reg = <0x4000 0x10>;
    clocks = <&cru CLK_USB2PHY_HDPTXRXPHY_REF>;
    clock-names = "phyclk";

    u2phy1_otg: otg-port {
        #phy-cells = <0>;
        status = "okay";
    };
};

// USB3 PHY (Combo DP + USB3)
usbdp_phy1: phy@fed90000 {
    compatible = "rockchip,rk3588-usbdp-phy";
    reg = <0x0 0xfed90000 0x0 0x10000>;
    rockchip,usb-grf = <&usb_grf>;
    rockchip,usbdpphy-grf = <&usbdpphy1_grf>;
    rockchip,vo-grf = <&vo0_grf>;
    clocks = <&cru CLK_USBDPPHY_MIPIDCPPHY_REF>,
             <&cru CLK_USBDP_PHY1_IMMORTAL>,
             <&cru PCLK_USBDPPHY1>;

    usbdp_phy1_u3: u3-port {
        #phy-cells = <0>;
        status = "okay";
    };
};
```

### 2.3 DWC3 核心：DesignWare 控制器初始化

**文件位置**：
- 核心: `drivers/usb/dwc3/core.c`
- Glue 层: `drivers/usb/dwc3/dwc3-generic.c`

**初始化流程**：

```c
// drivers/usb/dwc3/dwc3-generic.c:169
static int dwc3_generic_host_probe(struct udevice *dev)
{
    // 1. 调用通用 DWC3 probe
    rc = dwc3_generic_probe(dev, &priv->gen_priv);

    // 2. 映射寄存器地址
    hccr = (struct xhci_hccr *)priv->gen_priv.base;  // 0xfc000000
    hcor = (struct xhci_hcor *)(priv->gen_priv.base +
            HC_LENGTH(xhci_readl(&(hccr)->cr_capbase)));

    // 3. 注册 XHCI 控制器
    return xhci_register(dev, hccr, hcor);
}
```

**DWC3 核心初始化**：

```c
// drivers/usb/dwc3/dwc3-generic.c:50
static int dwc3_generic_probe(struct udevice *dev,
                              struct dwc3_generic_priv *priv)
{
    // 1. 解析设备树参数
    dwc3_of_parse(dwc3);

    // 2. RK3399/RK3588 特殊复位序列
    if (device_is_compatible(dev->parent, "rockchip,rk3399-dwc3")) {
        reset_assert_bulk(&glue->resets);  // 保持复位
        udelay(1);
    }

    // 3. PHY 初始化
    rc = dwc3_setup_phy(dev, &priv->phys, &priv->num_phys);

    // 4. 释放复位
    if (device_is_compatible(dev->parent, "rockchip,rk3399-dwc3"))
        reset_deassert_bulk(&glue->resets);

    // 5. 映射寄存器并初始化 DWC3 核心
    priv->base = map_physmem(plat->base, DWC3_OTG_REGS_END, MAP_NOCACHE);
    dwc3->regs = priv->base + DWC3_GLOBALS_REGS_START;  // 偏移 0xC100

    // 6. DWC3 核心初始化
    rc = dwc3_init(dwc3);
        ↓
        // drivers/usb/dwc3/core.c:795
        ret = dwc3_core_init(dwc);              // 全局寄存器配置
        ret = dwc3_event_buffers_setup(dwc);    // 事件缓冲区设置
        ret = dwc3_core_init_mode(dwc);         // 模式设置 (Host/Device)
}
```

**关键寄存器配置**：

```c
// drivers/usb/dwc3/core.c
static int dwc3_core_init(struct dwc3 *dwc)
{
    // 1. 软复位
    dwc3_core_soft_reset(dwc);

    // 2. 配置全局控制寄存器
    reg = dwc3_readl(dwc->regs, DWC3_GCTL);
    reg &= ~DWC3_GCTL_SCALEDOWN_MASK;       // 禁用降速
    reg &= ~DWC3_GCTL_DISSCRAMBLE;          // 使能 scrambler
    reg |= DWC3_GCTL_U2RSTECN;              // UTMI 复位使能

    // 3. 设置 USB2.0 PHY 接口宽度
    if (dwc->usb2_phyif_utmi_width == 16)
        reg |= DWC3_GCTL_PRTCAP_DIR(16);    // 16-bit UTMI+
    else
        reg |= DWC3_GCTL_PRTCAP_DIR(8);     // 8-bit UTMI+

    dwc3_writel(dwc->regs, DWC3_GCTL, reg);

    // 4. 配置总线配置寄存器
    reg = dwc3_readl(dwc->regs, DWC3_GBUSCFG);
    reg &= ~DWC3_GBUSCFG_DATACHAINLIMIT(~0);
    reg |= DWC3_GBUSCFG_DATACHAINLIMIT(4);  // 数据链限制
    dwc3_writel(dwc->regs, DWC3_GBUSCFG, reg);

    return 0;
}
```

### 2.4 XHCI 层：主机控制器初始化

**文件位置**：`drivers/usb/host/xhci.c`

**初始化流程**：

```c
// drivers/usb/host/xhci.c
int xhci_register(struct udevice *dev, struct xhci_hccr *hccr, struct xhci_hcor *hcor)
{
    struct xhci_ctrl *ctrl = dev_get_priv(dev);

    // 1. 保存寄存器基址
    ctrl->hccr = hccr;
    ctrl->hcor = hcor;

    // 2. 低级初始化
    ret = xhci_lowlevel_init(ctrl);
        ↓
        // 1. 控制器复位
        xhci_reset(hcor);
            ↓
            // 等待控制器停止
            // 发送复位命令
            // 等待复位完成

        // 2. 读取控制器能力
        xhci_readl(&hccr->cr_hcsparams1);  // MaxSlots, MaxIntrs
        xhci_readl(&hccr->cr_hcsparams2);  // MaxPorts
        xhci_readl(&hccr->cr_capbase);     // Capability pointer

        // 3. 内存初始化
        ret = xhci_mem_init(ctrl);
            ↓
            // 分配设备上下文数组
            ctrl->devs = calloc(sizeof(struct xhci_slot), HCS_MAX_SLOTS());

            // 分配并初始化命令环
            xhci_ring_alloc(ctrl, &ctrl->cmd_ring, 1, false);

            // 分配并初始化事件环
            xhci_ring_alloc(ctrl, &ctrl->event_ring, ERST_NUM_SEGS, false);

            // 分配端点上下文
            for (i = 0; i < HCS_MAX_SLOTS(); i++)
                xhci_alloc_device_context(ctrl, i);

        // 4. 启动控制器
        ret = xhci_run(ctrl);
            ↓
            // 1. 设置操作寄存器
            temp = xhci_readl(&ctrl->hcor->or_dnctrl);
            xhci_writel(&ctrl->hcor->or_dnctrl, temp);  // 禁用端口断电

            // 2. 配置所有端口为路由
            for (i = 0; i < HCS_MAX_PORTS(); i++) {
                temp = xhci_readl(&ctrl->hcor->or_portsc[i]);
                temp |= PORT_ROUTE;               // XHCI 路由
                xhci_writel(&ctrl->hcor->or_portsc[i], temp);
            }

            // 3. 启用控制器
            temp = xhci_readl(&ctrl->hcor->or_usbsts);
            if (temp & STS_HCH) {
                temp = xhci_readl(&ctrl->hcor->or_usbcmd);
                temp |= CMD_RUN;                  // 设置 RUN 位
                xhci_writel(&ctrl->hcor->or_usbcmd, temp);
            }

            // 4. 等待控制器就绪
            ret = xhci_wait_for_bit(&ctrl->hcor->or_usbsts,
                                    STS_HCH, false,
                                    XHCI_MAX_HALT_USEC, false);

    return 0;
}
```

### 2.5 命令层：USB 子系统启动

**文件位置**：
- 命令解析: `cmd/usb.c`
- 子系统初始化: `drivers/usb/host/usb-uclass.c`

**命令处理**：

```c
// cmd/usb.c:635
static int do_usb(cmd_tbl_t *cmdtp, int flag, int argc, char * const argv[])
{
    if (strncmp(argv[1], "start", 5) == 0) {
        if (usb_started)
            return 0; /* Already started */

        printf("starting USB...\n");
        do_usb_start();  // 调用启动函数
        return 0;
    }
    // ... 其他命令处理
}
```

**启动函数**：

```c
// cmd/usb.c:583
static void do_usb_start(void)
{
    bootstage_mark_name(BOOTSTAGE_ID_USB_START, "usb_start");

    // 1. USB 子系统初始化
    if (usb_init() < 0)
        return;

    // 2. 自动识别存储设备
#ifdef CONFIG_USB_STORAGE
    usb_stor_curr_dev = usb_stor_scan(1);
#endif

    // 3. 以太网设备扫描
#ifdef CONFIG_USB_HOST_ETHER
    usb_ether_curr_dev = usb_host_eth_scan(1);
#endif
}
```

**USB 子系统初始化**（Driver Model 模式）：

```c
// drivers/usb/host/usb-uclass.c:242
int usb_init(void)
{
    asynch_allowed = 1;

    // 1. 获取 USB uclass
    ret = uclass_get(UCLASS_USB, &uc);

    // 2. 探测每个 USB 控制器
    uclass_foreach_dev(bus, uc) {
        printf("Bus %s: ", bus->name);

        ret = device_probe(bus);  // 触发 DWC3/XHCI 初始化
        if (ret) {
            printf("probe failed, error %d\n", ret);
            continue;
        }

        controllers_initialized++;
        usb_started = true;
    }

    // 3. 扫描总线上的设备
    uclass_foreach_dev(bus, uc) {
        if (!device_active(bus))
            continue;

        priv = dev_get_uclass_priv(bus);
        if (!priv->companion)  // 主控制器
            usb_scan_bus(bus, true);
    }

    // 4. 扫描伴生控制器 (如果有)
    if (uc_priv->companion_device_count) {
        uclass_foreach_dev(bus, uc) {
            if (!device_active(bus))
                continue;

            priv = dev_get_uclass_priv(bus);
            if (priv->companion)
                usb_scan_bus(bus, true);
        }
    }

    // 5. 移除不活跃的设备
    remove_inactive_children(uc, bus);

    return usb_started ? 0 : -1;
}
```

---

## 三、设备发现和枚举流程

### 3.1 Root Hub 创建

**Root Hub** 是每个 USB 控制器内部的虚拟 Hub，所有 USB 设备都通过 Root Hub 连接。

**创建流程**：

```c
// drivers/usb/host/usb-uclass.c
int usb_scan_bus(struct udevice *bus, bool recurse)
{
    struct usb_uclass_priv *uc_priv = bus->uclass->priv;
    struct usb_bus_priv *priv = dev_get_uclass_priv(bus);
    struct udevice *dev;

    // 1. 为总线创建 Root Hub
    ret = usb_scan_device(bus, 0, USB_SPEED_FULL, &dev);
        ↓
        // drivers/usb/host/usb-uclass.c:540
        int usb_scan_device(struct udevice *bus, int port,
                          enum usb_speed speed, struct udevice **devp)
        {
            struct usb_device *udev;
            struct udevice *dev;

            // 1. 分配 USB 设备结构
            udev = usb_device_alloc();

            // 2. XHCI: Enable Slot 命令
            ret = usb_alloc_device(udev);
                ↓
                // xHCI 特定: 分配设备槽位
                // 发送 Enable Slot TRB 到命令环
                // 等待完成事件
                // 返回 slot_id

            // 3. 设置设备 (分配地址, 读取描述符)
            ret = usb_setup_device(udev, NULL, port, true);

            // 4. 选择配置
            ret = usb_select_config(udev);

            // 5. 创建 udevice 并绑定驱动
            ret = usb_device_create_dev(udev, &dev);

            return 0;
        }

    return 0;
}
```

### 3.2 Hub 端口扫描

**Hub 配置**：

```c
// drivers/usb/host/usb-uclass.c
static int usb_hub_configure(struct udevice *dev)
{
    struct usb_device *udev = dev_get_parent_priv(dev);
    struct usb_hub_device *hub = dev_get_priv(dev);
    int ret;

    // 1. 获取 Hub 描述符
    ret = usb_get_hub_descriptor(udev, buffer, length);

    // 2. 解析端口数量
    hub->desc.bNbrPorts = descriptor->bNbrPorts;

    // 3. USB3.0 Hub: 设置 Hub 深度
    if (usb_hub_is_superspeed(udev)) {
        ret = usb_set_hub_depth(udev, depth);
    }

    // 4. 端口上电
    usb_hub_power_on(hub);
        ↓
        for (i = 0; i < hub->desc.bNbrPorts; i++) {
            // 给每个端口上电
            usb_set_port_feature(udev, i + 1, USB_PORT_FEAT_POWER);

            // 等待电压稳定
            mdelay(hub->desc.bPwrOn2PwrGood * 2);
        }

    // 5. 创建扫描列表
    for (i = 0; i < hub->desc.bNbrPorts; i++) {
        struct usb_device_scan *usb_scan;
        usb_scan = calloc(1, sizeof(*usb_scan));
        usb_scan->dev = dev;
        usb_scan->hub = hub;
        usb_scan->port = i;
        list_add_tail(&usb_scan->list, &usb_scan_list);
    }

    // 6. 开始扫描
    ret = usb_device_list_scan();

    return 0;
}
```

### 3.3 设备检测

**端口扫描**：

```c
// drivers/usb/host/usb-uclass.c
static int usb_scan_port(struct usb_device_scan *usb_scan)
{
    struct usb_device *dev = usb_scan->dev;
    struct usb_hub_device *hub = usb_scan->hub;
    int port = usb_scan->port;
    int ret;

    // 1. 等待端口稳定
    if (get_timer(0) < hub->query_delay)
        return 0;

    // 2. 读取端口状态
    ret = usb_get_port_status(dev, port + 1, portsts);
    portstatus = le16_to_cpu(portsts->wPortStatus);
    portchange = le16_to_cpu(portsts->wPortChange);

    // 3. 检查连接变化
    if (!(portchange & USB_PORT_STAT_C_CONNECTION)) {
        // 没有连接变化
        if (!(portstatus & USB_PORT_STAT_CONNECTION)) {
            // 超时: 移除扫描项
            list_del(&usb_scan->list);
            return 0;
        }
    }

    // 4. 检测到设备连接
    if (portchange & USB_PORT_STAT_C_CONNECTION) {
        // 清除连接变化标志
        usb_clear_port_feature(dev, port + 1, USB_PORT_FEAT_C_CONNECTION);

        // 端口复位
        ret = usb_hub_port_reset(dev, port + 1, &portstatus);

        // 确定设备速度
        switch (portstatus & USB_PORT_STAT_SPEED_MASK) {
        case USB_PORT_STAT_SUPER_SPEED:  // USB3.0 (5Gbps)
            speed = USB_SPEED_SUPER;
            break;
        case USB_PORT_STAT_HIGH_SPEED:   // USB2.0 HS (480Mbps)
            speed = USB_SPEED_HIGH;
            break;
        case USB_PORT_STAT_LOW_SPEED:    // USB1.1 LS (1.5Mbps)
            speed = USB_SPEED_LOW;
            break;
        default:
            speed = USB_SPEED_FULL;      // USB1.1 FS (12Mbps)
        }

        // 5. 扫描新设备
        ret = usb_scan_device(dev->dev, port + 1, speed, &child);

        // 6. 移除扫描项
        list_del(&usb_scan->list);
    }

    return 0;
}
```

### 3.4 设备枚举

**设备设置**：

```c
// common/usb.c 或 drivers/usb/host/usb-uclass.c
int usb_setup_device(struct usb_device *dev, struct usb_device *parent,
                    int portnr, bool do_read)
{
    // 1. 准备设备
    ret = usb_prepare_device(dev, addr, do_read, parent);
        ↓
        // 1.1 分配设备上下文 (xHCI)
        err = usb_alloc_device(dev);
            ↓
            // xHCI: Enable Slot 命令
            struct xhci_ctrl *ctrl = dev->controller;
            ret = xhci_alloc_slot(ctrl, &slot_id);  // 返回 slot_id

        // 1.2 设置设备描述符
        err = usb_setup_descriptor(dev, do_read);
            ↓
            // 读取设备描述符的前 8 字节
            err = usb_get_descriptor(dev, USB_DT_DEVICE, 0, desc, 8);

            // 设置 max packet size
            dev->descriptor.bMaxPacketSize0 = desc->bMaxPacketSize0;

        // 1.3 端口复位
        err = usb_hub_port_reset(dev, parent);
            ↓
            // 发送端口复位请求
            usb_set_port_feature(parent, portnr, USB_PORT_FEAT_RESET);

            // 等待复位完成
            mdelay(50);

        // 1.4 分配设备地址
        dev->devnum = addr + 1;
        err = usb_set_address(dev);
            ↓
            // 控制传输: SET_ADDRESS 请求
            struct devreq setup;
            setup.bRequest = USB_REQ_SET_ADDRESS;
            setup.wValue = cpu_to_le16(dev->devnum);

            err = usb_control_msg(dev, usb_snddefctrl(dev),
                                  USB_REQ_SET_ADDRESS,
                                  USB_TYPE_STANDARD | USB_RECIP_DEVICE,
                                  dev->devnum, 0, NULL, 0,
                                  USB_CNTL_TIMEOUT);

        // 1.5 读取完整设备描述符 (如果之前只读了8字节)
        if (!do_read) {
            err = usb_setup_descriptor(dev, true);
        }

    // 2. 选择配置
    ret = usb_select_config(dev);
        ↓
        // 2.1 获取配置描述符长度
        err = get_descriptor_len(dev, USB_DT_CONFIG_SIZE, 9);

        // 2.2 获取完整配置描述符
        err = usb_get_configuration_len(dev, 0);
        err = usb_get_configuration_no(dev, 0, tmpbuf, err);

        // 2.3 解析配置
        usb_parse_config(dev, tmpbuf, 0);
            ↓
            // 解析配置描述符
            // 解析接口描述符
            // 解析端点描述符

        // 2.4 设置配置
        err = usb_set_configuration(dev, dev->config.desc.bConfigurationValue);
            ↓
            // 控制传输: SET_CONFIGURATION 请求
            err = usb_control_msg(dev, usb_snddefctrl(dev),
                                  USB_REQ_SET_CONFIGURATION,
                                  USB_TYPE_STANDARD | USB_RECIP_DEVICE,
                                  config, 0, NULL, 0,
                                  USB_CNTL_TIMEOUT);

        // 2.5 读取字符串描述符
        usb_string(dev, dev->descriptor.iManufacturer, dev->mf, ...);
        usb_string(dev, dev->descriptor.iProduct, dev->prod, ...);

    return 0;
}
```

### 3.5 Hub 递归扫描

**递归处理**：

```c
// drivers/usb/host/usb-uclass.c
static int usb_hub_probe(struct udevice *dev)
{
    // 1. 配置 Hub
    ret = usb_hub_configure(dev);

    // 2. 扫描 Hub 的每个端口
    for (i = 0; i < hub->desc.bNbrPorts; i++) {
        ret = usb_scan_port(&usb_scan[i]);

        // 3. 如果子设备也是 Hub，递归扫描
        if (child_device_is_hub) {
            usb_hub_probe(child_device);  // 递归调用
        }
    }

    return 0;
}
```

---

## 四、RK3588 特定实现细节

### 4.1 设备树配置

**USB3 控制器配置**（`rk3588s.dtsi`）：

```dts
usbdrd3_0: usbdrd3_0 {
    compatible = "rockchip,rk3588-dwc3", "rockchip,rk3399-dwc3";
    clocks = <&cru REF_CLK_USB3OTG0>,   // 参考时钟
             <&cru SUSPEND_CLK_USB3OTG0>, // 挂起时钟
             <&cru ACLK_USB3OTG0>;         // 总线时钟
    clock-names = "ref", "suspend", "bus";

    #address-cells = <2>;
    #size-cells = <2>;
    ranges;  // 地址映射
    status = "disabled";

    usbdrd_dwc3_0: usb@fc000000 {
        compatible = "snps,dwc3";
        reg = <0x0 0xfc000000 0x0 0x400000>;  // 4MB 寄存器空间
        interrupts = <GIC_SPI 220 IRQ_TYPE_LEVEL_HIGH>;
        power-domains = <&power RK3588_PD_USB>;

        resets = <&cru SRST_A_USB3OTG0>;
        reset-names = "usb3-otg";

        dr_mode = "otg";              // OTG 模式
        phy_type = "utmi_wide";       // UTMI+ 宽总线 (16-bit)

        // 各种 Quirks (硬件 bug 修复)
        snps,dis_enblslpm_quirk;          // 禁用 LPM
        snps,dis-u1-entry-quirk;          // 禁用 U1 状态
        snps,dis-u2-entry-quirk;          // 禁用 U2 状态
        snps,dis-u2-freeclk-exists-quirk; // U2 free clock quirk
        snps,dis-del-phy-power-chg-quirk; // PHY power change quirk
        snps,dis-tx-ipgap-linecheck-quirk; // TX IP gap quirk

        phys = <&u2phy0_otg>, <&usbdp_phy0_u3>;  // USB2 + USB3 PHY
        phy-names = "usb2-phy", "usb3-phy";

        status = "disabled";
    };
};
```

**PHY 系统配置**：

```dts
// USB2 PHY (用于 USB2.0/USB1.1 设备)
u2phy0: usb2-phy@0 {
    compatible = "rockchip,rk3588-usb2phy";
    reg = <0x0 0x0 0x0 0x10>;
    clocks = <&cru CLK_USB2PHY_HDPTXRXPHY_REF>;
    clock-names = "phyclk";
    #clock-cells = <0>;

    u2phy0_otg: otg-port {
        #phy-cells = <0>;
        status = "okay";
    };
};

// USB DP Combo PHY (USB3.0 + DisplayPort)
usbdp_phy0: phy@fed80000 {
    compatible = "rockchip,rk3588-usbdp-phy";
    reg = <0x0 0xfed80000 0x0 0x10000>;
    rockchip,usb-grf = <&usb_grf>;          // USB GRF @ 0xfd5ac000
    rockchip,usbdpphy-grf = <&usbdpphy0_grf>; // USBDP PHY GRF
    rockchip,vo-grf = <&vo0_grf>;
    clocks = <&cru CLK_USBDPPHY_MIPIDCPPHY_REF>,
             <&cru CLK_USBDP_PHY0_IMMORTAL>,
             <&cru PCLK_USBDPPHY0>;
    clock-names = "refclk", "immortal", "pclk";

    resets = <&cru SRST_USBDP_COMBO_PHY0_INIT>,
             <&cru SRST_USBDP_COMBO_PHY0_CMN>,
             <&cru SRST_USBDP_COMBO_PHY0_LANE>,
             <&cru SRST_USBDP_COMBO_PHY0_PCS>,
             <&cru SRST_P_USBDPPHY0>;
    reset-names = "init", "cmn", "lane", "pcs_apb", "pma_apb";

    status = "okay";

    usbdp_phy0_u3: u3-port {
        #phy-cells = <0>;
        status = "okay";
    };
};
```

**板级配置**（`orangepi5plus.dts`）：

```dts
&usbdrd3_0 {
    status = "okay";
};

&usbdrd_dwc3_0 {
    dr_mode = "host";         // 强制 Host 模式
    status = "okay";
};

&u2phy0 {
    status = "okay";
};

&u2phy0_otg {
    status = "okay";
};

&usbdp_phy0 {
    status = "okay";
};

&usbdp_phy0_u3 {
    status = "okay";
};
```

### 4.2 RK3588 特殊初始化序列

**Rockchip 特定复位序列**：

```c
// drivers/usb/dwc3/dwc3-generic.c:69
/*
 * 必须在整个 USB3.0 OTG 控制器保持复位的条件下
 * 才能在 RK3399 平台上初始化 TypeC PHY 时
 * 保持 pipe power state 处于 P2 状态。
 */
if (device_is_compatible(dev->parent, "rockchip,rk3399-dwc3")) {
    reset_assert_bulk(&glue->resets);  // 断言复位 (保持复位状态)
    udelay(1);                          // 短暂延迟
}

// PHY 初始化
rc = dwc3_setup_phy(dev, &priv->phys, &priv->num_phys);

if (device_is_compatible(dev->parent, "rockchip,rk3399-dwc3"))
    reset_deassert_bulk(&glue->resets); // 解除复位
```

**PHY 状态机**：

```
复位状态 (P3)
    ↓ (PHY 初始化)
Power State P2 (PLL 未锁定, 收发器禁用)
    ↓ (PLL 锁定)
Power State P0 (正常工作, 信号传输)
```

### 4.3 时钟和复位依赖

**时钟层次**：

```
XIN24M (24MHz 振荡器)
    ↓
CRU (Clock and Reset Unit)
    ├─ REF_CLK_USB3OTG0    → DWC3 参考时钟
    ├─ SUSPEND_CLK_USB3OTG0→ DWC3 挂起时钟
    ├─ ACLK_USB3OTG0       → DWC3 AHB 总线时钟
    ├─ CLK_USB2PHY_REF     → USB2 PHY 参考时钟
    ├─ CLK_USBDPPHY_REF    → USBDP PHY 参考时钟
    └─ CLK_USBDP_IMMORTAL  → USBDP PHY 常开时钟
```

**复位序列**：

```c
// 1. 初始化阶段
reset_assert_bulk(&glue->resets);  // 断言所有复位

// 2. PHY 初始化
dwc3_setup_phy(...)                // PHY 初始化

// 3. 控制器初始化
dwc3_core_init(...)                // DWC3 核心初始化

// 4. 释放复位
reset_deassert_bulk(&glue->resets); // 解除复位
```

**复位源**：

```c
struct reset_ctl_bulk {
    struct reset_ctl resets[3];
    int count;
};

// SRST_A_USB3OTG0: USB3 OTG 控制器 AHB 复位
// SRST_P_USB3OTG0: USB3 OTG 控制器 Por 复位
// SRST_USB3OTG0_UTMI: USB3 OTG UTMI 复位
```

### 4.4 电源域管理

**电源域**：

```dts
power-domains = <&power RK3588_PD_USB>;
```

**电源域状态**：

```
Power OFF
    ↓ (电源域使能)
Power ON
    ↓ (时钟使能)
Clock ON
    ↓ (复位解除)
Reset Release
    ↓ (控制器初始化)
Active
```

---

## 五、关键数据结构

### 5.1 DWC3 控制器结构

```c
// drivers/usb/dwc3/core.h
struct dwc3 {
    struct udevice          *dev;
    void __iomem            *regs;      // 寄存器基址
    void __iomem            *base;      // 物理基址

    enum usb_dr_mode        dr_mode;    // DR 模式 (Host/Device/Otg)
    enum usb_maximum_speed  maximum_speed; // 最大速度
    enum usb_phy_interface  hsphy_mode; // HS PHY 模式

    struct dwc3_event_buffer *ev_buf;   // 事件缓冲区
    struct dwc3_ep          eps[DWC3_ENDPOINTS_NUM]; // 端点数组

    u32                     speed;      // 当前速度
    u32                     hwparams[HWPARAMS_SIZE]; // 硬件参数

    struct list_head        list;       // DWC3 列表
};
```

### 5.2 XHCI 控制器结构

```c
// drivers/usb/host/xhci.h
struct xhci_ctrl {
    struct xhci_hccr        *hccr;      // 能力寄存器
    struct xhci_hcor        *hcor;      // 操作寄存器

    struct xhci_slot        *devs;      // 设备槽位数组
    struct xhci_ring        *cmd_ring;  // 命令环
    struct xhci_ring        *event_ring; // 事件环

    int                     rootdev;    // Root Hub 设备号
    u32                     hci_version; // HCI 版本

    struct usb_device       *usb3_dev;  // USB3.0 Root Hub
    struct usb_device       *usb2_dev;  // USB2.0 Root Hub
};

// 设备槽位
struct xhci_slot {
    struct xhci_ep_context  *ep_context; // 端点上下文
    struct xhci_device_context *dev_ctx; // 设备上下文
    struct usb_device       *udev;      // USB 设备
    int                     enabled;    // 是否使能
};

// TRB 环
struct xhci_ring {
    union xhci_trb          *trbs;      // TRB 数组
    int                     num_segs;   // 段数
    int                     enqueue;    // 入队指针
    int                     dequeue;    // 出队指针
    bool                    is_td;      // 是否是 TD
};
```

### 5.3 USB 设备结构

```c
// common/usb.h
struct usb_device {
    int                     devnum;     // 设备地址
    int                     speed;      // 速度
    int                     maxchild;   // 最大子设备数
    struct usb_device       *children[USB_MAXCHILDREN]; // 子设备数组
    struct usb_device       *parent;    // 父设备

    struct usb_device_descriptor descriptor;  // 设备描述符
    struct usb_config               config;   // 配置描述符

    struct udevice          *dev;       // udevice 实例
    void                    *controller; // 控制器指针
    int                     portnr;     // 端口号
};
```

### 5.4 Hub 设备结构

```c
// drivers/usb/host/usb-uclass.c
struct usb_hub_device {
    struct usb_hub_descriptor desc;     // Hub 描述符
    struct usb_device       *usb_dev;   // USB 设备
    int                     query_delay; // 查询延迟
};

// Hub 描述符
struct usb_hub_descriptor {
    u8  bDescLength;         // 描述符长度
    u8  bDescriptorType;     // 描述符类型
    u8  bNbrPorts;           // 端口数量
    u16 wHubCharacteristics; // Hub 特性
    u8  bPwrOn2PwrGood;      // 上电到好的延迟
    u8  bHubContrCurrent;    // Hub 控制电流
    // ... 更多字段
};
```

---

## 六、调试和诊断

### 6.1 常见初始化失败点

| 阶段 | 失败原因 | 诊断方法 |
|------|----------|----------|
| PHY 初始化 | 时钟未使能 | 检查时钟树配置 |
| PHY 初始化 | 复位未解除 | 检查复位控制逻辑 |
| DWC3 核心 | 寄存器访问失败 | 检查内存映射 |
| XHCI 启动 | 控制器未停止 | 检查 HCHalted 位 |
| 设备枚举 | 地址分配失败 | 检查 Enable Slot 命令 |
| 描述符读取 | 超时 | 检查 cable 连接和速度匹配 |

### 6.2 日志输出

**启用 USB 调试日志**：

```bash
# u-boot 环境变量
set usb_debug 1
set usb_init_debug 1

# 或在代码中
#define DEBUG
#define USB_DEBUG
```

**关键日志点**：

```c
// DWC3 初始化
dev_dbg(dwc->dev, "dwc3_core_init: %d\n", ret);

// XHCI 初始化
debug("xhci_init: hccr=%p, hcor=%p\n", hccr, hcor);

// 设备枚举
printf("New USB device %d is %s speed\n",
       dev->devnum, usb_speed_str(dev->speed));
```

### 6.3 寄存器状态检查

**DWC3 关键寄存器**：

```c
// 全局状态寄存器 (GSTS)
u32 gsts = dwc3_readl(dwc->regs, DWC3_GSTS);
if (gsts & DWC3_GSTS_CURMOD(1)) {
    // Device mode
} else if (gsts & DWC3_GSTS_CURMOD(2)) {
    // Host mode
}

// 设备状态寄存器 (DSTS)
u32 dsts = dwc3_readl(dwc->regs, DWC3_DSTS);
u32 speed = DSTS_CONNECTSPD(dsts);  // 连接速度
```

**XHCI 关键寄存器**：

```c
// USB 状态寄存器 (USBSTS)
u32 usbsts = xhci_readl(&ctrl->hcor->or_usbsts);
if (usbsts & STS_HCH) {
    // 控制器已停止
}

// 端口状态寄存器 (PORTSC)
u32 portsc = xhci_readl(&ctrl->hcor->or_portsc[port]);
if (portsc & PORT_CONNECT) {
    // 设备已连接
}
u32 speed = (portsc & PORT_SPEED_MASK) >> PORT_SPEED_SHIFT;
```

### 6.4 性能优化

**优化点**：

1. **减少延迟**
   - 优化 `mdelay()` 调用
   - 使用事件驱动而非轮询

2. **DMA 对齐**
   - 确保 DMA 缓冲区 64 字节对齐
   - 使用 cache-coherent 内存

3. **中断聚合**
   - 启用 XHCI 中断聚合
   - 减少中断处理开销

4. **TRB 批处理**
   - 合并多个传输到同一 TRB 链
   - 减少命令环占用

---

## 七、总结

### 7.1 完整流程图

```
[用户] usb start
  ↓
[命令层] do_usb() → do_usb_start()
  ↓
[USB子系统] usb_init()
  ├─ 控制器探测阶段
  │   ├─ device_probe()
  │   ├─ dwc3_generic_host_probe()
  │   │   ├─ 时钟使能
  │   │   ├─ 复位控制 (assert)
  │   │   ├─ PHY 初始化
  │   │   │   ├─ USB2 PHY 初始化
  │   │   │   └─ USB3 PHY 初始化
  │   │   ├─ 复位控制 (deassert)
  │   │   ├─ dwc3_init()
  │   │   │   ├─ dwc3_core_init()
  │   │   │   └─ dwc3_core_init_mode()
  │   │   └─ xhci_register()
  │   │       ├─ xhci_reset()
  │   │       ├─ xhci_mem_init()
  │   │       │   ├─ 分配设备槽位
  │   │       │   ├─ 创建命令环
  │   │       │   └─ 创建事件环
  │   │       └─ xhci_run()
  │   └─ 设备扫描阶段
  │       ├─ usb_scan_bus()
  │       ├─ usb_scan_device()
  │       │   ├─ usb_alloc_device() [XHCI Enable Slot]
  │       │   ├─ usb_setup_device()
  │       │   │   ├─ usb_prepare_device()
  │       │   │   │   ├─ 端口复位
  │       │   │   │   ├─ usb_set_address()
  │       │   │   │   └─ 读取设备描述符
  │       │   │   └─ usb_select_config()
  │       │   │       ├─ 读取配置描述符
  │       │   │       ├─ 解析接口/端点
  │       │   │       └─ usb_set_configuration()
  │       │   └─ usb_device_create_dev()
  │       └─ usb_hub_probe()
  │           ├─ usb_hub_configure()
  │           │   ├─ 获取 Hub 描述符
  │           │   ├─ 端口上电
  │           │   └─ 创建扫描列表
  │           └─ usb_scan_port()
  │               └─ 递归扫描子设备
  └─ 完成
```

### 7.2 关键依赖关系

```
USB 设备发现
    ↓ 依赖
XHCI 主机控制器
    ↓ 依赖
DWC3 DesignWare 核心
    ↓ 依赖
DWC3 Glue 层
    ↓ 依赖
PHY 层 (USB2 + USB3)
    ↓ 依赖
时钟和复位控制
    ↓ 依赖
硬件寄存器 (GRF, USBGRF)
    ↓ 依赖
RK3588 硬件
```

### 7.3 关键文件清单

| 层级 | 文件路径 | 功能 |
|------|----------|------|
| 命令层 | `cmd/usb.c` | USB 命令实现 |
| 子系统 | `drivers/usb/host/usb-uclass.c` | USB 子系统核心 |
| DWC3 核心 | `drivers/usb/dwc3/core.c` | DWC3 控制器核心 |
| DWC3 Glue | `drivers/usb/dwc3/dwc3-generic.c` | 平台 Glue 层 |
| XHCI | `drivers/usb/host/xhci.c` | XHCI 主机控制器 |
| XHCI 内存 | `drivers/usb/host/xhci-mem.c` | 内存管理 |
| XHCI 环 | `drivers/usb/host/xhci-ring.c` | TRB 环管理 |
| USB2 PHY | `drivers/phy/rockchip/phy-rockchip-naneng-usb2.c` | USB2 PHY 驱动 |
| USB3 PHY | `drivers/phy/rockchip/phy-rockchip-inno-usb3.c` | USB3 PHY 驱动 |
| USBDP PHY | `drivers/phy/rockchip/phy-rockchip-usbdp.c` | USB DP Combo PHY |
| 设备树 | `arch/arm/dts/rk3588s.dtsi` | RK3588 设备树 |
| 设备树 | `arch/arm/dts/rk3588.dtsi` | RK3588 扩展设备树 |
| 板级配置 | `orangepi/orangepi5plus.dts` | OrangePi 5 Plus 配置 |

### 7.4 参考资料

1. **RK3588 TRM** (Technical Reference Manual)
   - 章节: USB3.0 DRD Controller
   - 章节: USB PHY Configuration

2. **DWC3 数据手册**
   - Synopsis DesignWare Cores USB3.0 Controller
   - 寄存器定义和编程指南

3. **XHCI 规范**
   - USB 3.0 eXtensible Host Controller Interface
   - Intel xHCI Specification 1.1

4. **USB 规范**
   - USB 3.1 Specification
   - USB 2.0 Specification

---

## 附录：快速参考

### A. 设备发现关键步骤

1. **控制器初始化** (T0-T7)
   - 时钟使能
   - PHY 初始化
   - DWC3 配置
   - XHCI 启动

2. **Root Hub 创建** (T8)
   - 虚拟 Hub 设备
   - 端口上电

3. **设备检测** (T9-T10)
   - 端口状态轮询
   - 连接变化检测
   - 速度识别

4. **设备枚举** (T11)
   - 地址分配
   - 描述符读取
   - 配置设置

5. **Hub 递归** (T12)
   - 子 Hub 扫描
   - 多级设备树

### B. USB3.0 vs USB2.0 速度

| 规格 | 速度 | 带宽 |
|------|------|------|
| USB3.0 SuperSpeed | 5Gbps | 500MB/s |
| USB2.0 High Speed | 480Mbps | 60MB/s |
| USB1.1 Full Speed | 12Mbps | 1.5MB/s |
| USB1.1 Low Speed | 1.5Mbps | 187.5KB/s |

### C. RK3588 USB 控制器映射

| 控制器 | 基地址 | 模式 | 状态 |
|--------|--------|------|------|
| usbdrd3_0 | 0xfc000000 | OTG | 支持 |
| usbdrd3_1 | 0xfc400000 | Host | 支持 |
| usbhost3_0 | 0xfcd00000 | Host | 支持 |

---

**文档版本**: 1.0
**最后更新**: 2026-01-07
**适用平台**: RK3588 (Orange Pi 5 Plus)
**u-boot 版本**: 2024.10
