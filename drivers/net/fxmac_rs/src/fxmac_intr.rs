//! Interrupt handling for FXMAC Ethernet controller.
//!
//! This module provides interrupt handlers and ISR setup functions for
//! handling TX/RX completion, errors, and link status changes.

use crate::{FXmacIrqStatus, fxmac::*, fxmac_const::*, fxmac_dma::*};

pub(crate) fn FXmacIntrHandlerWithStatus(instance_p: &mut FXmac, status: FXmacIrqStatus) {
    assert!(instance_p.is_ready == FT_COMPONENT_IS_READY);

    let tx_queue_id = instance_p.tx_bd_queue.queue_id;
    let rx_queue_id = instance_p.rx_bd_queue.queue_id;

    assert!(
        (rx_queue_id < instance_p.config.max_queue_num)
            && (tx_queue_id < instance_p.config.max_queue_num)
    );

    // This ISR will try to handle as many interrupts as it can in a single
    // call. However, in most of the places where the user's error handler
    // is called, this ISR exits because it is expected that the user will
    // reset the device in nearly all instances.
    let mut reg_isr: u32 = status.raw();

    info!(
        "+++++++++ Interrupt Status ISR={:#x}, tx_queue_id={}, rx_queue_id={}",
        reg_isr, tx_queue_id, rx_queue_id
    );

    if tx_queue_id == 0 {
        if (reg_isr & FXMAC_IXR_TXCOMPL_MASK) != 0 {
            // Clear TX status register TX complete indication but preserve error bits if there is any
            write_reg(
                (instance_p.config.base_address + FXMAC_TXSR_OFFSET) as *mut u32,
                FXMAC_TXSR_TXCOMPL_MASK | FXMAC_TXSR_USEDREAD_MASK,
            );

            FXmacSendHandler(instance_p);
        }

        // Transmit error conditions interrupt
        if ((reg_isr & FXMAC_IXR_TX_ERR_MASK) != 0) && ((reg_isr & FXMAC_IXR_TXCOMPL_MASK) == 0) {
            // Clear TX status register
            let reg_txsr: u32 =
                read_reg((instance_p.config.base_address + FXMAC_TXSR_OFFSET) as *const u32);

            write_reg(
                (instance_p.config.base_address + FXMAC_TXSR_OFFSET) as *mut u32,
                reg_txsr,
            );

            FXmacErrorHandler(instance_p, FXMAC_SEND as u8, reg_txsr);
        }

        // add restart
        if (reg_isr & FXMAC_IXR_TXUSED_MASK) != 0 {
            // if (instance_p->restart_handler)
            // {
            // instance_p->restart_handler(instance_p->restart_args);
            // }
        }

        // link changed
        if (reg_isr & FXMAC_IXR_LINKCHANGE_MASK) != 0 {
            FXmacLinkChange(instance_p);
        }
    } else {
        reg_isr = read_reg(
            (instance_p.config.base_address
                + FXMAC_QUEUE_REGISTER_OFFSET(FXMAC_INTQ1_STS_OFFSET, tx_queue_id))
                as *const u32,
        );

        // Transmit Q1 complete interrupt
        if ((reg_isr & FXMAC_INTQUESR_TXCOMPL_MASK) != 0) {
            // Clear TX status register TX complete indication but preserve
            // error bits if there is any
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

        // Transmit Q1 error conditions interrupt
        if (((reg_isr & FXMAC_INTQ1SR_TXERR_MASK) != 0)
            && ((reg_isr & FXMAC_INTQ1SR_TXCOMPL_MASK) != 0))
        {
            // Clear Interrupt Q1 status register
            write_reg(
                (instance_p.config.base_address
                    + FXMAC_QUEUE_REGISTER_OFFSET(FXMAC_INTQ1_STS_OFFSET, tx_queue_id))
                    as *mut u32,
                reg_isr,
            );

            FXmacErrorHandler(instance_p, FXMAC_SEND as u8, reg_isr);
        }
    }

    if rx_queue_id == 0 {
        // Receive complete interrupt
        if (reg_isr & FXMAC_IXR_RXCOMPL_MASK) != 0 {
            // Clear RX status register RX complete indication but preserve
            // error bits if there is any
            write_reg(
                (instance_p.config.base_address + FXMAC_RXSR_OFFSET) as *mut u32,
                FXMAC_RXSR_FRAMERX_MASK | FXMAC_RXSR_BUFFNA_MASK,
            );
            FXmacRecvIsrHandler(instance_p);
        }

        // Receive error conditions interrupt
        if (reg_isr & FXMAC_IXR_RX_ERR_MASK) != 0 {
            // Clear RX status register
            let mut reg_rxsr: u32 =
                read_reg((instance_p.config.base_address + FXMAC_RXSR_OFFSET) as *const u32);
            write_reg(
                (instance_p.config.base_address + FXMAC_RXSR_OFFSET) as *mut u32,
                reg_rxsr,
            );

            if (reg_isr & FXMAC_IXR_RXUSED_MASK) != 0 {
                let reg_ctrl: u32 =
                    read_reg((instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *const u32);

                let mut reg_temp: u32 = reg_ctrl | FXMAC_NWCTRL_FLUSH_DPRAM_MASK;
                reg_temp &= !FXMAC_NWCTRL_RXEN_MASK;
                write_reg(
                    (instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *mut u32,
                    reg_temp,
                );

                // add
                reg_temp = reg_ctrl | FXMAC_NWCTRL_RXEN_MASK;
                write_reg(
                    (instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *mut u32,
                    reg_temp,
                );
            }

            if reg_rxsr != 0 {
                FXmacErrorHandler(instance_p, FXMAC_RECV as u8, reg_rxsr);
            }
        }
    } else {
        // use queue number more than 0
        reg_isr = read_reg(
            (instance_p.config.base_address
                + FXMAC_QUEUE_REGISTER_OFFSET(FXMAC_INTQ1_STS_OFFSET, rx_queue_id))
                as *const u32,
        );

        // Receive complete interrupt
        if ((reg_isr & FXMAC_INTQUESR_RCOMP_MASK) != 0) {
            // Clear RX status register RX complete indication but preserve
            // error bits if there is any
            write_reg(
                (instance_p.config.base_address
                    + FXMAC_QUEUE_REGISTER_OFFSET(FXMAC_INTQ1_STS_OFFSET, rx_queue_id))
                    as *mut u32,
                FXMAC_INTQUESR_RCOMP_MASK,
            );
            FXmacRecvIsrHandler(instance_p);
        }

        // Receive error conditions interrupt
        if (reg_isr & FXMAC_IXR_RX_ERR_MASK) != 0 {
            let mut reg_ctrl: u32 =
                read_reg((instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *const u32);
            reg_ctrl &= !FXMAC_NWCTRL_RXEN_MASK;

            write_reg(
                (instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *mut u32,
                reg_ctrl,
            );

            // Clear RX status register
            let mut reg_rxsr =
                read_reg((instance_p.config.base_address + FXMAC_RXSR_OFFSET) as *const u32);
            write_reg(
                (instance_p.config.base_address + FXMAC_RXSR_OFFSET) as *mut u32,
                reg_rxsr,
            );

            if ((reg_isr & FXMAC_IXR_RXUSED_MASK) != 0) {
                reg_ctrl =
                    read_reg((instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *const u32);
                reg_ctrl |= FXMAC_NWCTRL_FLUSH_DPRAM_MASK;

                write_reg(
                    (instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *mut u32,
                    reg_ctrl,
                );
            }

            // Clear RX status register RX complete indication but preserve
            // error bits if there is any
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

/// @name: FXmacQueueIrqDisable
/// @msg:  Disable queue irq
/// @param {FXmac} *instance_p a pointer to the instance to be worked on.
/// @param {u32} queue_num queue number
/// @param {u32} mask is interrupt disable value mask
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
                    // FXmacRecvHandler(instance_p);
                }
                if (error_word & FXMAC_RXSR_BUFFNA_MASK) != 0 {
                    error!("Receive buffer not available");
                    // FXmacRecvHandler(instance_p);
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
