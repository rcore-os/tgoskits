use alloc::boxed::Box;
use core::task::Context;

use usb_if::{
    descriptor::EndpointType,
    endpoint::{EndpointInfo, RequestId, TransferCompletion, TransferRequest},
    err::TransferError,
    host::hub::Speed,
};

use crate::{
    backend::kmod::{
        Kernel,
        dwc2::{
            channel::{
                ChannelConfig, HostChannelPool, iso::IsoChannelState, non_iso::NonIsoChannelState,
            },
            endpoint_type_to_dwc2,
            reg::Dwc2Registers,
            stats::Dwc2Stats,
        },
    },
    err::Result,
};

pub(crate) struct Dwc2Endpoint {
    config: ChannelConfig,
    non_iso: NonIsoChannelState,
    iso: IsoChannelState,
}

unsafe impl Send for Dwc2Endpoint {}

pub(crate) struct Dwc2EndpointParams {
    pub(crate) regs: Dwc2Registers,
    pub(crate) kernel: Kernel,
    pub(crate) device_address: u8,
    pub(crate) port_speed: Speed,
    pub(crate) info: EndpointInfo,
    pub(crate) channel_pool: HostChannelPool,
    pub(crate) stats: Dwc2Stats,
}

impl Dwc2Endpoint {
    pub(crate) fn new(params: Dwc2EndpointParams) -> Result<Self> {
        let Dwc2EndpointParams {
            regs,
            kernel,
            device_address,
            port_speed,
            info,
            channel_pool,
            stats,
        } = params;
        endpoint_type_to_dwc2(info.transfer_type)?;
        let config = ChannelConfig {
            device_address,
            info,
            port_speed,
        };
        Ok(Self {
            config,
            non_iso: NonIsoChannelState::new(
                regs,
                kernel.clone(),
                stats.clone(),
                channel_pool.clone(),
            ),
            iso: IsoChannelState::new(regs, kernel, stats, channel_pool),
        })
    }

    pub(crate) fn set_device_address(&mut self, address: u8) {
        self.config.device_address = address;
    }

    pub(crate) fn set_max_packet_size(&mut self, max_packet_size: u8) {
        self.config.info.max_packet_size = u16::from(max_packet_size).max(8);
    }

    /// 在飞或已完成的请求 id（`Dwc2Device::quiesce_endpoints` 停稳前查询）。
    pub(crate) fn in_flight_request_id(&self) -> Option<RequestId> {
        match self.config.info.transfer_type {
            EndpointType::Isochronous => self.iso.in_flight_request_id(),
            _ => self.non_iso.in_flight_request_id(),
        }
    }
}

impl crate::backend::ty::ep::EndpointOp for Dwc2Endpoint {
    fn submit_request(
        &mut self,
        request: TransferRequest,
    ) -> core::result::Result<RequestId, TransferError> {
        // 通道由各状态机内部从池中租借（ISO 常驻会话，non-ISO 按请求）。
        if matches!(request, TransferRequest::Isochronous { .. }) {
            return self.iso.submit(&self.config, request);
        }
        self.non_iso.submit(&self.config, request)
    }

    fn reclaim_request(
        &mut self,
        id: RequestId,
    ) -> Option<core::result::Result<TransferCompletion, TransferError>> {
        match self.config.info.transfer_type {
            EndpointType::Isochronous => self.iso.reclaim(id),
            _ => self.non_iso.reclaim(id),
        }
    }

    fn register_waker(&self, id: RequestId, cx: &mut Context<'_>) {
        match self.config.info.transfer_type {
            EndpointType::Isochronous => self.iso.register_waker(id, cx),
            _ => self.non_iso.register_waker(id, cx),
        }
    }

    fn cancel_request(&mut self, id: RequestId) -> core::result::Result<(), TransferError> {
        match self.config.info.transfer_type {
            EndpointType::Isochronous => self.iso.cancel(id),
            _ => self.non_iso.cancel(id),
        }
    }

    fn reset(&mut self) -> crate::backend::ty::ep::EndpointResetFuture {
        let result = match self.config.info.transfer_type {
            EndpointType::Isochronous => self.iso.reset(),
            _ => self.non_iso.reset(),
        };
        Box::pin(async move { result })
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::sync::Arc;

    use tock_registers::interfaces::{Readable, Writeable};
    use usb_if::{
        descriptor::EndpointType,
        endpoint::{EndpointAddress, EndpointInfo, TransferRequest},
        host::hub::Speed,
        transfer::Direction,
    };

    use super::*;
    use crate::{
        backend::{
            kmod::dwc2::{
                channel::{Dwc2ChannelCompletions, Dwc2PeriodicSchedule, HostChannelPool},
                event::Dwc2EventHandler,
                reg::{GINTSTS_DISCONNINT, HCINT_CHHLTD, HCINT_NAK, HCINT_XFERCOMPL},
                stats::Dwc2Stats,
                testutil as tu,
            },
            ty::{Event, EventHandlerOp, ep::EndpointOp},
        },
        osal::Kernel,
    };

    fn channel_pool(
        channels: u8,
        kernel: &Kernel,
        completions: &Dwc2ChannelCompletions,
    ) -> HostChannelPool {
        HostChannelPool::new(
            channels,
            completions.clone(),
            Arc::new(Dwc2PeriodicSchedule::new(kernel).unwrap()),
        )
    }

    fn bulk_endpoint(regs: Dwc2Registers, kernel: Kernel, pool: HostChannelPool) -> Dwc2Endpoint {
        match Dwc2Endpoint::new(Dwc2EndpointParams {
            regs,
            kernel,
            device_address: 2,
            port_speed: Speed::High,
            info: EndpointInfo {
                address: EndpointAddress::new(0x81),
                transfer_type: EndpointType::Bulk,
                direction: Direction::In,
                max_packet_size: 512,
                packets_per_microframe: 1,
                interval: 0,
            },
            channel_pool: pool,
            stats: Dwc2Stats::new(),
        }) {
            Ok(endpoint) => endpoint,
            Err(err) => panic!("DWC2 test endpoint creation failed: {err:?}"),
        }
    }

    #[test]
    fn submit_waits_for_irq_completion_before_reclaiming() {
        let (_backing, regs) = tu::test_regs();
        let kernel = tu::test_kernel();
        let completions = Dwc2ChannelCompletions::new();
        let channel_pool = channel_pool(2, &kernel, &completions);
        let mut endpoint = bulk_endpoint(regs, kernel, channel_pool);
        let mut data = [0u8; 512];
        let id = endpoint
            .submit_request(TransferRequest::bulk_in(&mut data))
            .unwrap();

        // 只开 CHHLTD（DDMA 下 XFERCOMPL/NAK 不单独中断）。
        assert_eq!(regs.regs().hc[1].hcintmsk.get(), HCINT_CHHLTD);
        assert_eq!(
            regs.regs().hc[1].hcintmsk.get() & (HCINT_NAK | HCINT_XFERCOMPL),
            0
        );
        assert!(endpoint.reclaim_request(id).is_none());

        completions.publish(1, HCINT_CHHLTD | HCINT_XFERCOMPL);
        assert!(endpoint.reclaim_request(id).is_some());
    }

    #[test]
    fn cancelled_endpoint_waits_for_real_channel_halt_before_reclaiming() {
        let (_backing, regs) = tu::test_regs();
        let kernel = tu::test_kernel();
        let completions = Dwc2ChannelCompletions::new();
        let channel_pool = channel_pool(2, &kernel, &completions);
        let mut endpoint = bulk_endpoint(regs, kernel, channel_pool);
        let mut data = [0u8; 512];
        let id = endpoint
            .submit_request(TransferRequest::bulk_in(&mut data))
            .unwrap();

        endpoint.cancel_request(id).unwrap();

        // 取消投递 CHDIS 而非立即回收；真实 CHHLTD 到达前 reclaim 为空。
        assert_eq!(regs.regs().hc[1].hcchar.get() & (1 << 30), 1 << 30);
        assert!(endpoint.reclaim_request(id).is_none());

        completions.publish(1, HCINT_CHHLTD);
        assert!(matches!(
            endpoint.reclaim_request(id),
            Some(Err(TransferError::Cancelled))
        ));
    }

    #[test]
    fn disconnect_completes_active_request_without_more_channel_writes() {
        let (_backing, regs) = tu::test_regs();
        let kernel = tu::test_kernel();
        let completions = Dwc2ChannelCompletions::new();
        let channel_pool = channel_pool(2, &kernel, &completions);
        let mut endpoint = bulk_endpoint(regs, kernel, channel_pool);
        let handler = Dwc2EventHandler::new(regs, completions.clone(), Dwc2Stats::new());
        let mut data = [0u8; 512];
        let id = endpoint
            .submit_request(TransferRequest::bulk_in(&mut data))
            .unwrap();

        regs.regs().gintmsk.set(GINTSTS_DISCONNINT);
        regs.regs().gintsts.set(GINTSTS_DISCONNINT);
        assert!(matches!(handler.handle_event(), Event::Stopped));
        let channel_value = regs.regs().hc[1].hcchar.get();

        endpoint.cancel_request(id).unwrap();

        assert_eq!(regs.regs().hc[1].hcchar.get(), channel_value);
        assert!(matches!(
            endpoint.reclaim_request(id),
            Some(Err(TransferError::Disconnected))
        ));
    }
}
