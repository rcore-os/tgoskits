//! Interrupt handling for FXMAC Ethernet controller.
//!
//! This module provides interrupt handlers and ISR setup functions for
//! handling TX/RX completion, errors, and link status changes.

use core::sync::atomic::AtomicPtr;
use core::sync::atomic::Ordering;

use alloc::boxed::Box;

use crate::fxmac::*;
use crate::fxmac_const::*;
use crate::fxmac_dma::*;

/* XMAC */
pub const FXMAC_NUM: u32 = 4;
pub const FXMAC0_ID: u32 = 0;
pub const FXMAC1_ID: u32 = 1;
pub const FXMAC2_ID: u32 = 2;
pub const FXMAC3_ID: u32 = 3;
pub const FXMAC0_BASE_ADDR: u32 = 0x3200C000;
pub const FXMAC1_BASE_ADDR: u32 = 0x3200E000;
pub const FXMAC2_BASE_ADDR: u32 = 0x32010000;
pub const FXMAC3_BASE_ADDR: u32 = 0x32012000;
pub const FXMAC0_MODE_SEL_BASE_ADDR: u32 = 0x3200DC00;
pub const FXMAC0_LOOPBACK_SEL_BASE_ADDR: u32 = 0x3200DC04;
pub const FXMAC1_MODE_SEL_BASE_ADDR: u32 = 0x3200FC00;
pub const FXMAC1_LOOPBACK_SEL_BASE_ADDR: u32 = 0x3200FC04;
pub const FXMAC2_MODE_SEL_BASE_ADDR: u32 = 0x32011C00;
pub const FXMAC2_LOOPBACK_SEL_BASE_ADDR: u32 = 0x32011C04;
pub const FXMAC3_MODE_SEL_BASE_ADDR: u32 = 0x32013C00;
pub const FXMAC3_LOOPBACK_SEL_BASE_ADDR: u32 = 0x32013C04;
pub const FXMAC0_PCLK: u32 = 50000000;
pub const FXMAC1_PCLK: u32 = 50000000;
pub const FXMAC2_PCLK: u32 = 50000000;
pub const FXMAC3_PCLK: u32 = 50000000;
pub const FXMAC0_HOTPLUG_IRQ_NUM: u32 = (53 + 30);
pub const FXMAC1_HOTPLUG_IRQ_NUM: u32 = (54 + 30);
pub const FXMAC2_HOTPLUG_IRQ_NUM: u32 = (55 + 30);
pub const FXMAC3_HOTPLUG_IRQ_NUM: u32 = (56 + 30);
pub const FXMAC_QUEUE_MAX_NUM: u32 = 16; // #define FXMAC_QUEUE_MAX_NUM 16U
pub const FXMAC0_QUEUE0_IRQ_NUM: u32 = (57 + 30);
pub const FXMAC0_QUEUE1_IRQ_NUM: u32 = (58 + 30);
pub const FXMAC0_QUEUE2_IRQ_NUM: u32 = (59 + 30);
pub const FXMAC0_QUEUE3_IRQ_NUM: u32 = (60 + 30);
pub const FXMAC0_QUEUE4_IRQ_NUM: u32 = (30 + 30);
pub const FXMAC0_QUEUE5_IRQ_NUM: u32 = (31 + 30);
pub const FXMAC0_QUEUE6_IRQ_NUM: u32 = (32 + 30);
pub const FXMAC0_QUEUE7_IRQ_NUM: u32 = (33 + 30);
pub const FXMAC1_QUEUE0_IRQ_NUM: u32 = (61 + 30);
pub const FXMAC1_QUEUE1_IRQ_NUM: u32 = (62 + 30);
pub const FXMAC1_QUEUE2_IRQ_NUM: u32 = (63 + 30);
pub const FXMAC1_QUEUE3_IRQ_NUM: u32 = (64 + 30);
pub const FXMAC2_QUEUE0_IRQ_NUM: u32 = (66 + 30);
pub const FXMAC2_QUEUE1_IRQ_NUM: u32 = (67 + 30);
pub const FXMAC2_QUEUE2_IRQ_NUM: u32 = (68 + 30);
pub const FXMAC2_QUEUE3_IRQ_NUM: u32 = (69 + 30);
pub const FXMAC3_QUEUE0_IRQ_NUM: u32 = (70 + 30);
pub const FXMAC3_QUEUE1_IRQ_NUM: u32 = (71 + 30);
pub const FXMAC3_QUEUE2_IRQ_NUM: u32 = (72 + 30);
pub const FXMAC3_QUEUE3_IRQ_NUM: u32 = (73 + 30);
//pub const FXMAC_PHY_MAX_NUM:u32 = 32;
// #define FXMAC_CLK_TYPE_0

/// Global pointer to the active FXMAC instance.
///
/// This atomic pointer is set during initialization and used by the interrupt
/// handler to access the controller instance.
pub static XMAC: AtomicPtr<FXmac> = AtomicPtr::new(core::ptr::null_mut());

/// Top-level interrupt handler for FXMAC.
///
/// This function should be registered as the interrupt handler for the FXMAC
/// controller. It retrieves the global FXMAC instance and dispatches to the
/// appropriate sub-handlers.
///
/// # Safety
///
/// This function accesses the global `XMAC` pointer. It assumes that the
/// pointer has been properly initialized by [`xmac_init`].
pub fn xmac_intr_handler() {
    debug!("Handling xmac intr ...");

    let xmac = XMAC.load(Ordering::Relaxed);
    if !xmac.is_null() {
        let xmac_ptr = unsafe { &mut (*xmac) };

        // maybe irq num
        let vector = xmac_ptr.config.queue_irq_num[0];
        FXmacIntrHandler(vector as i32, xmac_ptr);

        info!("xmac intr is already handled");
    } else {
        error!("static FXmac has not been initialized");
    }
}

/// Main interrupt handler for FXMAC controller.
///
/// Processes all pending interrupts for the specified FXMAC instance. This
/// handler supports the following interrupt types:
///
/// - **FXMAC_HANDLER_DMARECV**: RX completion - calls `FXmacRecvIsrHandler`
/// - **FXMAC_HANDLER_DMASEND**: TX completion - calls `FXmacSendHandler`
/// - **FXMAC_HANDLER_ERROR**: Error conditions - calls `FXmacErrorHandler`
/// - **FXMAC_HANDLER_LINKCHANGE**: Link status change - calls `FXmacLinkChange`
///
/// # Arguments
///
/// * `vector` - The IRQ vector number that triggered the interrupt.
/// * `instance_p` - Mutable reference to the FXMAC instance.
///
/// # Note
///
/// Currently only single-queue operation is fully supported.
pub fn FXmacIntrHandler(vector: i32, instance_p: &mut FXmac) {
    assert!(instance_p.is_ready == FT_COMPONENT_IS_READY);

    // 0 ~ FXMAC_QUEUE_MAX_NUM ,Index queue number
    let tx_queue_id = instance_p.tx_bd_queue.queue_id;
    // 0 ~ FXMAC_QUEUE_MAX_NUM ,Index queue number
    let rx_queue_id = instance_p.rx_bd_queue.queue_id;

    assert!((rx_queue_id < FXMAC_QUEUE_MAX_NUM) && (tx_queue_id < FXMAC_QUEUE_MAX_NUM));

    /* This ISR will try to handle as many interrupts as it can in a single
     * call. However, in most of the places where the user's error handler
     * is called, this ISR exits because it is expected that the user will
     * reset the device in nearly all instances.
     */
    let mut reg_isr: u32 =
        read_reg((instance_p.config.base_address + FXMAC_ISR_OFFSET) as *const u32);

    info!(
        "+++++++++ IRQ num vector={}, Interrupt Status ISR={:#x}, tx_queue_id={}, rx_queue_id={}",
        vector, reg_isr, tx_queue_id, rx_queue_id
    );

    if vector as u32 == instance_p.config.queue_irq_num[tx_queue_id as usize] {
        if tx_queue_id == 0 {
            if (reg_isr & FXMAC_IXR_TXCOMPL_MASK) != 0 {
                // Clear TX status register TX complete indication but preserve error bits if there is any
                write_reg(
                    (instance_p.config.base_address + FXMAC_TXSR_OFFSET) as *mut u32,
                    FXMAC_TXSR_TXCOMPL_MASK | FXMAC_TXSR_USEDREAD_MASK,
                );

                FXmacSendHandler(instance_p);

                /* add */
                if (instance_p.caps & FXMAC_CAPS_ISR_CLEAR_ON_WRITE) != 0 {
                    write_reg(
                        (instance_p.config.base_address + FXMAC_ISR_OFFSET) as *mut u32,
                        FXMAC_IXR_TXCOMPL_MASK,
                    );
                }
            }

            /* Transmit error conditions interrupt */
            if ((reg_isr & FXMAC_IXR_TX_ERR_MASK) != 0) && ((reg_isr & FXMAC_IXR_TXCOMPL_MASK) == 0)
            {
                /* Clear TX status register */
                let reg_txsr: u32 =
                    read_reg((instance_p.config.base_address + FXMAC_TXSR_OFFSET) as *const u32);

                write_reg(
                    (instance_p.config.base_address + FXMAC_TXSR_OFFSET) as *mut u32,
                    reg_txsr,
                );

                FXmacErrorHandler(instance_p, FXMAC_SEND as u8, reg_txsr);

                /* add */
                if (instance_p.caps & FXMAC_CAPS_ISR_CLEAR_ON_WRITE) != 0 {
                    write_reg(
                        (instance_p.config.base_address + FXMAC_ISR_OFFSET) as *mut u32,
                        FXMAC_IXR_TX_ERR_MASK,
                    );
                }
            }

            /* add restart */
            if (reg_isr & FXMAC_IXR_TXUSED_MASK) != 0 {
                /* add */
                if (instance_p.caps & FXMAC_CAPS_ISR_CLEAR_ON_WRITE) != 0 {
                    write_reg(
                        (instance_p.config.base_address + FXMAC_ISR_OFFSET) as *mut u32,
                        FXMAC_IXR_TXUSED_MASK,
                    );
                }

                /*
                if (instance_p->restart_handler)
                {
                    instance_p->restart_handler(instance_p->restart_args);
                }
                */
            }

            /* link changed */
            if (reg_isr & FXMAC_IXR_LINKCHANGE_MASK) != 0 {
                FXmacLinkChange(instance_p);

                if (instance_p.caps & FXMAC_CAPS_ISR_CLEAR_ON_WRITE) != 0 {
                    write_reg(
                        (instance_p.config.base_address + FXMAC_ISR_OFFSET) as *mut u32,
                        FXMAC_IXR_LINKCHANGE_MASK,
                    );
                }
            }
        } else
        /* use queue number more than 0 */
        {
            reg_isr = read_reg(
                (instance_p.config.base_address
                    + FXMAC_QUEUE_REGISTER_OFFSET(FXMAC_INTQ1_STS_OFFSET, tx_queue_id))
                    as *const u32,
            );

            /* Transmit Q1 complete interrupt */
            if ((reg_isr & FXMAC_INTQUESR_TXCOMPL_MASK) != 0) {
                /* Clear TX status register TX complete indication but preserve
                 * error bits if there is any */
                write_reg(
                    (instance_p.config.base_address
                        + FXMAC_QUEUE_REGISTER_OFFSET(FXMAC_INTQ1_STS_OFFSET, tx_queue_id))
                        as *mut u32,
                    FXMAC_INTQUESR_TXCOMPL_MASK,
                );
                write_reg(
                    (instance_p.config.base_address + FXMAC_TXSR_OFFSET) as *mut u32,
                    FXMAC_TXSR_TXCOMPL_MASK | FXMAC_TXSR_USEDREAD_MASK,
                );

                FXmacSendHandler(instance_p);
            }

            /* Transmit Q1 error conditions interrupt */
            if (((reg_isr & FXMAC_INTQ1SR_TXERR_MASK) != 0)
                && ((reg_isr & FXMAC_INTQ1SR_TXCOMPL_MASK) != 0))
            {
                /* Clear Interrupt Q1 status register */
                write_reg(
                    (instance_p.config.base_address
                        + FXMAC_QUEUE_REGISTER_OFFSET(FXMAC_INTQ1_STS_OFFSET, tx_queue_id))
                        as *mut u32,
                    reg_isr,
                );

                FXmacErrorHandler(instance_p, FXMAC_SEND as u8, reg_isr);
            }
        }
    }

    if vector as u32 == instance_p.config.queue_irq_num[rx_queue_id as usize] {
        if rx_queue_id == 0 {
            /* Receive complete interrupt */
            if (reg_isr & FXMAC_IXR_RXCOMPL_MASK) != 0 {
                /* Clear RX status register RX complete indication but preserve
                 * error bits if there is any */
                write_reg(
                    (instance_p.config.base_address + FXMAC_RXSR_OFFSET) as *mut u32,
                    FXMAC_RXSR_FRAMERX_MASK | FXMAC_RXSR_BUFFNA_MASK,
                );
                FXmacRecvIsrHandler(instance_p);

                /* add */
                if (instance_p.caps & FXMAC_CAPS_ISR_CLEAR_ON_WRITE) != 0 {
                    write_reg(
                        (instance_p.config.base_address + FXMAC_ISR_OFFSET) as *mut u32,
                        FXMAC_IXR_RXCOMPL_MASK,
                    );
                }
            }

            /* Receive error conditions interrupt */
            if (reg_isr & FXMAC_IXR_RX_ERR_MASK) != 0 {
                /* Clear RX status register */
                let mut reg_rxsr: u32 =
                    read_reg((instance_p.config.base_address + FXMAC_RXSR_OFFSET) as *const u32);
                write_reg(
                    (instance_p.config.base_address + FXMAC_RXSR_OFFSET) as *mut u32,
                    reg_rxsr,
                );

                if (reg_isr & FXMAC_IXR_RXUSED_MASK) != 0 {
                    let reg_ctrl: u32 = read_reg(
                        (instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *const u32,
                    );

                    let mut reg_temp: u32 = reg_ctrl | FXMAC_NWCTRL_FLUSH_DPRAM_MASK;
                    reg_temp &= !FXMAC_NWCTRL_RXEN_MASK;
                    write_reg(
                        (instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *mut u32,
                        reg_temp,
                    );

                    /* add  */
                    reg_temp = reg_ctrl | FXMAC_NWCTRL_RXEN_MASK;
                    write_reg(
                        (instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *mut u32,
                        reg_temp,
                    );

                    if (instance_p.caps & FXMAC_CAPS_ISR_CLEAR_ON_WRITE) != 0 {
                        write_reg(
                            (instance_p.config.base_address + FXMAC_ISR_OFFSET) as *mut u32,
                            FXMAC_IXR_RXUSED_MASK,
                        );
                    }
                }

                /* add */
                if ((reg_isr & FXMAC_IXR_RXOVR_MASK) != 0)
                    && ((instance_p.caps & FXMAC_CAPS_ISR_CLEAR_ON_WRITE) != 0)
                {
                    write_reg(
                        (instance_p.config.base_address + FXMAC_ISR_OFFSET) as *mut u32,
                        FXMAC_IXR_RXOVR_MASK,
                    );
                }

                /* add */
                if ((reg_isr & FXMAC_IXR_HRESPNOK_MASK) != 0)
                    && ((instance_p.caps & FXMAC_CAPS_ISR_CLEAR_ON_WRITE) != 0)
                {
                    write_reg(
                        (instance_p.config.base_address + FXMAC_ISR_OFFSET) as *mut u32,
                        FXMAC_IXR_HRESPNOK_MASK,
                    );
                }

                if reg_rxsr != 0 {
                    FXmacErrorHandler(instance_p, FXMAC_RECV as u8, reg_rxsr);
                }
            }
        } else {
            /* use queue number more than 0 */
            reg_isr = read_reg(
                (instance_p.config.base_address
                    + FXMAC_QUEUE_REGISTER_OFFSET(FXMAC_INTQ1_STS_OFFSET, rx_queue_id))
                    as *const u32,
            );

            /* Receive complete interrupt */
            if ((reg_isr & FXMAC_INTQUESR_RCOMP_MASK) != 0) {
                /* Clear RX status register RX complete indication but preserve
                 * error bits if there is any */
                write_reg(
                    (instance_p.config.base_address
                        + FXMAC_QUEUE_REGISTER_OFFSET(FXMAC_INTQ1_STS_OFFSET, rx_queue_id))
                        as *mut u32,
                    FXMAC_INTQUESR_RCOMP_MASK,
                );
                FXmacRecvIsrHandler(instance_p);
            }

            /* Receive error conditions interrupt */
            if (reg_isr & FXMAC_IXR_RX_ERR_MASK) != 0 {
                let mut reg_ctrl: u32 =
                    read_reg((instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *const u32);
                reg_ctrl &= !FXMAC_NWCTRL_RXEN_MASK;

                write_reg(
                    (instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *mut u32,
                    reg_ctrl,
                );

                /* Clear RX status register */
                let mut reg_rxsr =
                    read_reg((instance_p.config.base_address + FXMAC_RXSR_OFFSET) as *const u32);
                write_reg(
                    (instance_p.config.base_address + FXMAC_RXSR_OFFSET) as *mut u32,
                    reg_rxsr,
                );

                if ((reg_isr & FXMAC_IXR_RXUSED_MASK) != 0) {
                    reg_ctrl = read_reg(
                        (instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *const u32,
                    );
                    reg_ctrl |= FXMAC_NWCTRL_FLUSH_DPRAM_MASK;

                    write_reg(
                        (instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *mut u32,
                        reg_ctrl,
                    );
                }

                /* Clear RX status register RX complete indication but preserve
                 * error bits if there is any */
                write_reg(
                    (instance_p.config.base_address
                        + FXMAC_QUEUE_REGISTER_OFFSET(FXMAC_INTQ1_STS_OFFSET, rx_queue_id))
                        as *mut u32,
                    FXMAC_INTQUESR_RXUBR_MASK,
                );
                FXmacRecvIsrHandler(instance_p);

                if reg_rxsr != 0 {
                    FXmacErrorHandler(instance_p, FXMAC_RECV as u8, reg_rxsr);
                }
            }
        }
    }
}

/**
 * @name: FXmacQueueIrqDisable
 * @msg:  Disable queue irq
 * @param {FXmac} *instance_p a pointer to the instance to be worked on.
 * @param {u32} queue_num queue number
 * @param {u32} mask is interrupt disable value mask
 */
pub fn FXmacQueueIrqDisable(instance_p: &mut FXmac, queue_num: u32, mask: u32) {
    assert!(instance_p.is_ready == FT_COMPONENT_IS_READY);
    assert!(instance_p.config.max_queue_num > queue_num);

    if queue_num == 0 {
        write_reg(
            (instance_p.config.base_address + FXMAC_IDR_OFFSET) as *mut u32,
            mask & FXMAC_IXR_ALL_MASK,
        );
    } else {
        write_reg(
            (instance_p.config.base_address + FXMAC_INTQX_IDR_SIZE_OFFSET(queue_num as u64))
                as *mut u32,
            mask & FXMAC_IXR_ALL_MASK,
        );
    }
}

/// FXmacQueueIrqEnable, Enable queue irq
pub fn FXmacQueueIrqEnable(instance_p: &mut FXmac, queue_num: u32, mask: u32) {
    assert!(instance_p.is_ready == FT_COMPONENT_IS_READY);
    assert!(instance_p.config.max_queue_num > queue_num);

    if queue_num == 0 {
        write_reg(
            (instance_p.config.base_address + FXMAC_IER_OFFSET) as *mut u32,
            mask & FXMAC_IXR_ALL_MASK,
        );
    } else {
        write_reg(
            (instance_p.config.base_address + FXMAC_INTQX_IER_SIZE_OFFSET(queue_num as u64))
                as *mut u32,
            mask & FXMAC_IXR_ALL_MASK,
        );
    }
}

pub fn FXmacErrorHandler(instance_p: &mut FXmac, direction: u8, error_word: u32) {
    debug!(
        "-> FXmacErrorHandler, direction={}, error_word={}",
        direction, error_word
    );
    if error_word != 0 {
        match direction as u32 {
            FXMAC_RECV => {
                if (error_word & FXMAC_RXSR_HRESPNOK_MASK) != 0 {
                    error!("Receive DMA error");
                    FXmacHandleDmaTxError(instance_p);
                }
                if (error_word & FXMAC_RXSR_RXOVR_MASK) != 0 {
                    error!("Receive over run");
                    //FXmacRecvHandler(instance_p);
                }
                if (error_word & FXMAC_RXSR_BUFFNA_MASK) != 0 {
                    error!("Receive buffer not available");
                    //FXmacRecvHandler(instance_p);
                }
            }
            FXMAC_SEND => {
                if (error_word & FXMAC_TXSR_HRESPNOK_MASK) != 0 {
                    error!("Transmit DMA error");
                    FXmacHandleDmaTxError(instance_p);
                }
                if (error_word & FXMAC_TXSR_URUN_MASK) != 0 {
                    error!("Transmit under run");
                    FXmacHandleTxErrors(instance_p);
                }
                if (error_word & FXMAC_TXSR_BUFEXH_MASK) != 0 {
                    error!("Transmit buffer exhausted");
                    FXmacHandleTxErrors(instance_p);
                }
                if (error_word & FXMAC_TXSR_RXOVR_MASK) != 0 {
                    error!("Transmit retry excessed limits");
                    FXmacHandleTxErrors(instance_p);
                }
                if (error_word & FXMAC_TXSR_FRAMERX_MASK) != 0 {
                    error!("Transmit collision");
                    FXmacProcessSentBds(instance_p);
                }
            }
            _ => {
                error!("FXmacErrorHandler failed, unknown direction={}", direction);
            }
        }
    }
}

pub fn FXmacRecvIsrHandler(instance: &mut FXmac) {
    debug!("-> FXmacRecvIsrHandler");
    // 关中断
    write_reg(
        (instance.config.base_address + FXMAC_IDR_OFFSET) as *mut u32,
        FXMAC_IXR_RXCOMPL_MASK,
    );
    instance.lwipport.recv_flg += 1;

    ethernetif_input_to_recv_packets(instance);
    // 处理后会开中断
}

/// 网卡中断设置
pub fn FXmacSetupIsr(instance: &mut FXmac) {
    // 获取当前CPU ID: 0, 1, 2, 3, 4, 5, 6, 7
    //let cpu_id: u32 = get_cpu_id();
    // 路由中断到指定的cpu，或所有的cpu

    // Setup callbacks， 为指定类型设置回调函数
    /*
    FXmacSetHandler(&instance_p->instance, FXMAC_HANDLER_DMASEND, FXmacSendHandler, instance_p);
    FXmacSetHandler(&instance_p->instance, FXMAC_HANDLER_DMARECV, FXmacRecvIsrHandler, instance_p);
    FXmacSetHandler(&instance_p->instance, FXMAC_HANDLER_ERROR, FXmacErrorHandler, instance_p);
    FXmacSetHandler(&instance_p->instance, FXMAC_HANDLER_LINKCHANGE, FXmacLinkChange, instance_p);
    */

    //let IRQ_PRIORITY_VALUE_0 = 0x0;
    //let IRQ_PRIORITY_VALUE_12 = 0xc;
    // 设置中断优先级为IRQ_PRIORITY_VALUE_12

    // setup interrupt handler, 该函数将自定义中断回调函数注册到对应的中断ID
    // 使能对应中断
    let irq_num = instance.config.queue_irq_num[0] as usize; // 32 + 55

    // SPI(Shared Peripheral Interrupt) rang: 32..1020
    info!("register callback function for irq: {}", irq_num);
    crate_interface::call_interface!(crate::KernelFunc::dma_request_irq(
        irq_num,
        xmac_intr_handler
    ));
}
