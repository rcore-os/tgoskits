use alloc::{collections::BTreeMap, sync::Arc, vec, vec::Vec};

use dma_api::DmaDirection;
use mbarrier::mb;
use spin::Mutex;
use usb_if::{
    descriptor::{self, EndpointDescriptor},
    endpoint::{RequestId, TransferCompletion, TransferRequest},
    err::TransferError,
    transfer::{BmRequestType, Direction},
};
use xhci::{
    registers::doorbell,
    ring::trb::{
        event::TransferEvent,
        transfer::{self, Isoch, Normal},
    },
};

use super::{DirectionExt, reg::SlotBell, ring::SendRing, transfer::TransferId};
use crate::{
    BusAddr,
    backend::{
        Dci,
        ty::{
            ep::{EndpointOp, transfer_to_completion},
            transfer::{Transfer, TransferKind},
        },
    },
    err::ConvertXhciError,
    osal::Kernel,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct EndpointRequestId(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ControlStage {
    Setup,
    Data,
    Status,
}

#[derive(Clone, Copy, Debug)]
struct ControlTd {
    setup_trb: TransferId,
    data_trb: Option<TransferId>,
    status_trb: TransferId,
    requested: usize,
    actual: Option<usize>,
}

impl ControlTd {
    fn trbs(self) -> impl Iterator<Item = (TransferId, ControlStage)> {
        [
            Some((self.setup_trb, ControlStage::Setup)),
            self.data_trb.map(|trb| (trb, ControlStage::Data)),
            Some((self.status_trb, ControlStage::Status)),
        ]
        .into_iter()
        .flatten()
    }

    fn register_waker(&self, ring: &SendRing<TransferEvent>, cx: &mut core::task::Context<'_>) {
        for (trb, _) in (*self).trbs() {
            ring.register_cx(trb.0, cx);
        }
    }
}

struct SubmittedTd {
    transfer: Transfer,
    kind: SubmittedTdKind,
    trb_count: usize,
    cancelled: bool,
}

#[derive(Clone)]
enum SubmittedTdKind {
    Normal { completion_trb: TransferId },
    Control(ControlTd),
    Iso { packets: Vec<IsoPacketTd> },
}

#[derive(Clone, Copy)]
struct IsoPacketTd {
    trb: TransferId,
    final_packet: bool,
    event: Option<TransferEvent>,
    actual: Option<usize>,
}

pub struct Endpoint {
    dci: Dci,
    pub ring: SendRing<TransferEvent>,
    bell: Arc<Mutex<SlotBell>>,
    next_request_id: u64,
    inflight: BTreeMap<EndpointRequestId, SubmittedTd>,
    trb_to_request: BTreeMap<TransferId, EndpointRequestId>,
    outstanding_trbs: usize,
    kernel: Kernel,
    max_packet_size: usize,
    max_burst_size: usize,
    interval: u8,
    iso_start_asap: bool,
    next_iso_frame_id: u16,
}

unsafe impl Send for Endpoint {}
unsafe impl Sync for Endpoint {}

const ENDPOINT_RING_PAGES: usize = 16;

impl Endpoint {
    pub fn new(dci: Dci, kernel: &Kernel, bell: Arc<Mutex<SlotBell>>) -> crate::err::Result<Self> {
        let ring =
            SendRing::new_with_pages(ENDPOINT_RING_PAGES, DmaDirection::Bidirectional, kernel)?;

        Ok(Self {
            dci,
            ring,
            bell,
            next_request_id: 1,
            inflight: BTreeMap::new(),
            trb_to_request: BTreeMap::new(),
            outstanding_trbs: 0,
            kernel: kernel.clone(),
            max_packet_size: 0,
            max_burst_size: 0,
            interval: 1,
            iso_start_asap: true,
            next_iso_frame_id: 0,
        })
    }

    pub fn configure_periodic(
        &mut self,
        max_packet_size: usize,
        max_burst_size: usize,
        interval: u8,
    ) {
        self.max_packet_size = max_packet_size;
        self.max_burst_size = max_burst_size;
        self.interval = interval.max(1);
        self.iso_start_asap = true;
    }

    pub fn bus_addr(&self) -> BusAddr {
        self.ring.bus_addr()
    }

    fn doorbell(&mut self) {
        let mut bell = doorbell::Register::default();
        bell.set_doorbell_target(self.dci.into());
        self.bell.lock().ring(bell);
    }

    pub fn ring(&self) -> &SendRing<TransferEvent> {
        &self.ring
    }

    fn allocate_request_id(&mut self) -> EndpointRequestId {
        loop {
            let id = EndpointRequestId(self.next_request_id);
            self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
            if !self.inflight.contains_key(&id) {
                return id;
            }
        }
    }

    fn public_request_id(id: EndpointRequestId) -> RequestId {
        RequestId::new(id.0)
    }

    fn private_request_id(id: RequestId) -> EndpointRequestId {
        EndpointRequestId(id.raw())
    }

    fn validate_completion_code(
        &self,
        event: TransferEvent,
        transfer: &Transfer,
    ) -> Result<(), TransferError> {
        let kind_name = match &transfer.kind {
            TransferKind::Control(_) => "control",
            TransferKind::Bulk => "bulk",
            TransferKind::Interrupt => "interrupt",
            TransferKind::Isochronous { .. } => "iso",
        };
        match event.completion_code() {
            Ok(code) => {
                if let Err(e) = code.to_result() {
                    warn!(
                        "xhci: transfer error dci={} kind={} code={:?} remaining={}",
                        self.dci.raw(),
                        kind_name,
                        code,
                        event.trb_transfer_length()
                    );
                    return Err(e);
                }
            }
            Err(e) => {
                warn!(
                    "xhci: transfer error dci={} kind={} unknown_code={} remaining={}",
                    self.dci.raw(),
                    kind_name,
                    e,
                    event.trb_transfer_length()
                );
                return Err(TransferError::Other(anyhow!("Transfer failed")));
            }
        }
        Ok(())
    }

    fn complete_request(
        &mut self,
        request_id: EndpointRequestId,
        event_trb: TransferId,
        event: TransferEvent,
    ) -> Result<Transfer, TransferError> {
        let submitted = self.inflight.remove(&request_id).ok_or_else(|| {
            warn!(
                "xhci: completion for missing request dci={} request_id={} event_trb={:#x}",
                self.dci.raw(),
                request_id.0,
                event_trb.0.raw()
            );
            TransferError::InvalidEndpoint
        })?;
        self.outstanding_trbs = self.outstanding_trbs.saturating_sub(submitted.trb_count);
        match &submitted.kind {
            SubmittedTdKind::Normal { completion_trb } => {
                self.trb_to_request.remove(completion_trb);
            }
            SubmittedTdKind::Control(control_td) => {
                for (trb, _) in (*control_td).trbs() {
                    self.trb_to_request.remove(&trb);
                }
            }
            SubmittedTdKind::Iso { packets } => {
                for packet in packets {
                    self.trb_to_request.remove(&packet.trb);
                }
            }
        }

        if submitted.cancelled {
            return Err(TransferError::Cancelled);
        }

        if !matches!(submitted.kind, SubmittedTdKind::Iso { .. }) {
            self.validate_completion_code(event, &submitted.transfer)?;
        }
        self.transfer_from_completion(submitted, event_trb, event)
    }

    fn transfer_from_completion(
        &mut self,
        submitted: SubmittedTd,
        event_trb: TransferId,
        event: TransferEvent,
    ) -> Result<Transfer, TransferError> {
        let mut transfer = submitted.transfer;
        if let SubmittedTdKind::Iso { packets } = &submitted.kind {
            let TransferKind::Isochronous { packet_lengths } = &transfer.kind else {
                return Err(TransferError::Other(anyhow!("non-ISO transfer has ISO TD")));
            };
            if packets.len() != packet_lengths.len() {
                return Err(TransferError::Other(anyhow!(
                    "ISO completion count mismatch: ids={}, packets={}",
                    packets.len(),
                    packet_lengths.len()
                )));
            }

            let mut actual_lengths = Vec::with_capacity(packets.len());
            for (index, packet) in packets.iter().copied().enumerate() {
                let requested = packet_lengths[index];
                let actual = match packet.actual {
                    Some(actual) => actual,
                    None if packet.trb == event_trb => iso_packet_actual_length(requested, event)?,
                    None => 0,
                };
                actual_lengths.push(actual);
            }

            let transfer_len = actual_lengths.iter().sum();
            transfer.iso_packet_actual_lengths = actual_lengths;
            if transfer_len > 0 && matches!(transfer.direction, Direction::In) {
                transfer.prepare_read_all();
            }
            transfer.transfer_len = transfer_len;
            trace!("ISO transfer data length: {}", transfer.transfer_len);
            return Ok(transfer);
        }

        let transfer_len = match submitted.kind {
            SubmittedTdKind::Control(control_td) => {
                control_td.actual.unwrap_or(control_td.requested)
            }
            SubmittedTdKind::Normal { .. } => {
                let remaining = event.trb_transfer_length() as usize;
                transfer.buffer_len().saturating_sub(remaining)
            }
            SubmittedTdKind::Iso { .. } => unreachable!("ISO was handled above"),
        };

        if transfer_len > 0 && matches!(transfer.direction, Direction::In) {
            transfer.prepare_read_all();
        }
        transfer.transfer_len = transfer_len;
        trace!("Transfer data length: {}", transfer.transfer_len);
        Ok(transfer)
    }

    fn reclaim_control_request(
        &mut self,
        id: RequestId,
        request_id: EndpointRequestId,
        control_td: ControlTd,
    ) -> Option<Result<TransferCompletion, TransferError>> {
        for (event_trb, stage) in control_td.trbs() {
            let Some(event) = self.ring.get_finished(event_trb.0) else {
                continue;
            };
            let remaining = event.trb_transfer_length() as usize;
            if let Some(submitted) = self.inflight.get_mut(&request_id)
                && let SubmittedTdKind::Control(control_td) = &mut submitted.kind
            {
                match stage {
                    ControlStage::Setup => {}
                    ControlStage::Data => {
                        control_td.actual =
                            Some(submitted.transfer.buffer_len().saturating_sub(remaining));
                    }
                    ControlStage::Status if control_td.actual.is_none() => {
                        control_td.actual = Some(control_td.requested);
                    }
                    ControlStage::Status => {}
                }
            }
            match event.completion_code() {
                Ok(code) if code.to_result().is_ok() => {
                    if matches!(stage, ControlStage::Status) {
                        return Some(
                            self.complete_request(request_id, event_trb, event)
                                .map(|transfer| transfer_to_completion(id, transfer)),
                        );
                    }
                }
                _ => {
                    return Some(
                        self.complete_request(request_id, event_trb, event)
                            .map(|transfer| transfer_to_completion(id, transfer)),
                    );
                }
            }
        }
        None
    }

    fn reclaim_iso_request(
        &mut self,
        id: RequestId,
        request_id: EndpointRequestId,
    ) -> Option<Result<TransferCompletion, TransferError>> {
        let packet_count = self.iso_packet_count(request_id)?;
        for index in 0..packet_count {
            if self.iso_packet_done(request_id, index) {
                continue;
            }
            let (packet_trb, requested) = self.iso_packet_info(request_id, index)?;

            let Some(event) = self.ring.get_finished(packet_trb.0) else {
                continue;
            };
            let actual = match iso_packet_actual_length(requested, event) {
                Ok(actual) => actual,
                Err(err) => {
                    let cleanup_result = self.complete_request(request_id, packet_trb, event);
                    let result = match cleanup_result {
                        Ok(_) => Err(err),
                        Err(cleanup_err) => Err(cleanup_err),
                    };
                    return Some(result.map(|transfer| transfer_to_completion(id, transfer)));
                }
            };

            let fatal = iso_packet_is_fatal(event);
            let should_complete =
                self.record_iso_packet(request_id, index, event, actual, fatal)?;
            if should_complete {
                return Some(
                    self.complete_request(request_id, packet_trb, event)
                        .map(|transfer| transfer_to_completion(id, transfer)),
                );
            }
        }
        None
    }

    fn iso_packet_count(&self, request_id: EndpointRequestId) -> Option<usize> {
        self.inflight
            .get(&request_id)
            .and_then(|submitted| match &submitted.kind {
                SubmittedTdKind::Iso { packets } => Some(packets.len()),
                _ => None,
            })
    }

    fn iso_packet_done(&self, request_id: EndpointRequestId, index: usize) -> bool {
        self.inflight
            .get(&request_id)
            .and_then(|submitted| match &submitted.kind {
                SubmittedTdKind::Iso { packets } => {
                    packets.get(index).map(|packet| packet.actual.is_some())
                }
                _ => None,
            })
            .unwrap_or(true)
    }

    fn iso_packet_info(
        &self,
        request_id: EndpointRequestId,
        index: usize,
    ) -> Option<(TransferId, usize)> {
        let submitted = self.inflight.get(&request_id)?;
        let SubmittedTdKind::Iso { packets } = &submitted.kind else {
            return None;
        };
        let packet = packets.get(index)?;
        let requested = match &submitted.transfer.kind {
            TransferKind::Isochronous { packet_lengths } => {
                packet_lengths.get(index).copied().unwrap_or(0)
            }
            _ => return None,
        };
        Some((packet.trb, requested))
    }

    fn record_iso_packet(
        &mut self,
        request_id: EndpointRequestId,
        index: usize,
        event: TransferEvent,
        actual: usize,
        fatal: bool,
    ) -> Option<bool> {
        let submitted = self.inflight.get_mut(&request_id)?;
        let SubmittedTdKind::Iso { packets } = &mut submitted.kind else {
            return None;
        };
        let final_packet = packets.get(index).is_some_and(|packet| packet.final_packet);
        if final_packet || fatal {
            for packet in packets.iter_mut().take(index) {
                if packet.actual.is_none() {
                    packet.actual = Some(0);
                }
            }
        }
        if let Some(packet) = packets.get_mut(index) {
            packet.event = Some(event);
            packet.actual = Some(actual);
        }
        Some(final_packet || fatal || packets.iter().all(|packet| packet.actual.is_some()))
    }

    fn enque_trb(&mut self, trb: transfer::Allowed) -> TransferId {
        TransferId(self.ring.enque_transfer(trb))
    }

    fn enque_iso(
        &mut self,
        bus_addr: u64,
        packet_lengths: &[usize],
        interrupt_on_short_packet: bool,
    ) -> Vec<IsoPacketTd> {
        let mut packets = Vec::with_capacity(packet_lengths.len().max(1));
        let mut offset = 0u64;
        let packet_count = packet_lengths.len().max(1);
        let interval = self.interval.max(1);
        let mut frame_id = self.next_iso_frame_id;

        for index in 0..packet_count {
            let packet_length = packet_lengths.get(index).copied().unwrap_or(0);
            let last_packet = index + 1 == packet_count;
            let trb = self.enque_iso_trb(
                bus_addr + offset,
                packet_length,
                last_packet,
                !last_packet,
                frame_id,
                interrupt_on_short_packet,
            );
            packets.push(IsoPacketTd {
                trb,
                final_packet: last_packet,
                event: None,
                actual: None,
            });
            offset += packet_length as u64;
            frame_id = frame_id.wrapping_add(interval as u16) & 0x7ff;
        }

        self.next_iso_frame_id = frame_id;
        packets
    }

    fn enque_iso_trb(
        &mut self,
        bus_addr: u64,
        buff_len: usize,
        last_packet: bool,
        block_event_interrupt: bool,
        frame_id: u16,
        interrupt_on_short_packet: bool,
    ) -> TransferId {
        let mut trb = Isoch::new();
        trb.set_data_buffer_pointer(bus_addr as _)
            .set_trb_transfer_length(buff_len as _)
            .set_interrupter_target(0);

        if self.use_iso_sia() {
            trb.set_start_isoch_asap();
        } else {
            trb.set_frame_id(frame_id & 0x7ff);
        }
        if interrupt_on_short_packet {
            trb.set_interrupt_on_short_packet();
        }
        let total_packets = if self.max_packet_size == 0 {
            1
        } else {
            buff_len.div_ceil(self.max_packet_size).max(1)
        };
        let packets_per_burst = self.max_burst_size.saturating_add(1).max(1);
        let burst_count = total_packets.div_ceil(packets_per_burst).saturating_sub(1);
        let last_burst_packet_count = match total_packets % packets_per_burst {
            0 => packets_per_burst.saturating_sub(1),
            residue => residue.saturating_sub(1),
        };
        trb.set_td_size_or_tbc(burst_count.min(0x1f) as u8)
            .set_transfer_last_burst_packet_count(last_burst_packet_count.min(0xf) as u8);
        trb.set_interrupt_on_completion();
        if block_event_interrupt && !last_packet {
            trb.set_block_event_interrupt();
        }

        let trb = transfer::Allowed::Isoch(trb);
        self.enque_trb(trb)
    }

    fn use_iso_sia(&self) -> bool {
        self.iso_start_asap
    }

    fn required_trbs(transfer: &Transfer) -> usize {
        match &transfer.kind {
            TransferKind::Control(_) => {
                if transfer.buffer_len() > 0 {
                    3
                } else {
                    2
                }
            }
            TransferKind::Bulk | TransferKind::Interrupt => 1,
            TransferKind::Isochronous { packet_lengths } => packet_lengths.len().max(1),
        }
    }

    fn ensure_ring_capacity(&self, required: usize) -> Result<(), TransferError> {
        let usable = self.ring.usable_capacity().saturating_sub(1);
        if self.outstanding_trbs.saturating_add(required) > usable {
            return Err(TransferError::QueueFull);
        }
        Ok(())
    }

    fn required_trbs_for_request(request: &TransferRequest) -> usize {
        match request {
            TransferRequest::Control { buffer, .. } => {
                if buffer.is_some_and(|buffer| buffer.len > 0) {
                    3
                } else {
                    2
                }
            }
            TransferRequest::Bulk { .. } | TransferRequest::Interrupt { .. } => 1,
            TransferRequest::Isochronous { packets, .. } => packets.len().max(1),
        }
    }
}

impl EndpointOp for Endpoint {
    fn submit_request(&mut self, request: TransferRequest) -> Result<RequestId, TransferError> {
        let required_trbs = Self::required_trbs_for_request(&request);
        self.ensure_ring_capacity(required_trbs)?;
        let transfer = Transfer::from_request(&self.kernel, request)?;
        debug_assert_eq!(required_trbs, Self::required_trbs(&transfer));

        let mut data_bus_addr = 0;
        if transfer.buffer_len() > 0 {
            if matches!(transfer.direction, Direction::Out) {
                transfer.confirm_write_all();
            }
            data_bus_addr = transfer.dma_addr();
            let buffer_end = data_bus_addr + transfer.buffer_len() as u64;
            if data_bus_addr > self.kernel.dma_mask() || buffer_end > self.kernel.dma_mask() {
                return Err(TransferError::Other(anyhow!(
                    "DMA buffer [{:#x}, {:#x}) exceeds controller DMA mask {:#x}",
                    data_bus_addr,
                    buffer_end,
                    self.kernel.dma_mask()
                )));
            }
        }

        let data_len = transfer.buffer_len();
        let dir = transfer.direction;
        let request_id = self.allocate_request_id();

        let kind = match &transfer.kind {
            TransferKind::Control(t) => {
                let bm_request_type = BmRequestType {
                    direction: transfer.direction,
                    request_type: t.request_type,
                    recipient: t.recipient,
                };

                let mut setup = transfer::SetupStage::default();
                setup
                    .set_request_type(bm_request_type.into())
                    .set_request(t.request.into())
                    .set_value(t.value)
                    .set_index(t.index)
                    .set_length(0)
                    .set_transfer_type(transfer::TransferType::No);

                let mut data = None;

                if transfer.buffer_len() > 0 {
                    setup
                        .set_transfer_type(dir.to_xhci_transfer_type())
                        .set_length(data_len as _);

                    let mut _data = transfer::DataStage::default();
                    _data
                        .set_data_buffer_pointer(data_bus_addr)
                        .set_trb_transfer_length(data_len as _)
                        .set_direction(transfer.direction.to_xhci_direction());
                    if matches!(transfer.direction, Direction::In) {
                        _data.set_interrupt_on_short_packet();
                    }
                    data = Some(_data);
                }

                let mut status = transfer::StatusStage::default();
                status.set_interrupt_on_completion();

                if matches!(transfer.direction, Direction::In) && transfer.buffer_len() > 0 {
                    status.clear_direction();
                } else {
                    status.set_direction();
                }

                let has_data_stage = data.is_some();
                let mut control_trbs: Vec<transfer::Allowed> = vec![setup.into()];
                if let Some(data) = data {
                    control_trbs.push(data.into());
                }
                control_trbs.push(status.into());

                let control_addrs = self.ring.enqueue_transfer_td(&mut control_trbs);
                let mut control_addrs = control_addrs.into_iter();
                let setup_trb = TransferId(
                    control_addrs
                        .next()
                        .expect("control TD must contain setup TRB"),
                );
                let data_trb = if has_data_stage {
                    Some(TransferId(
                        control_addrs
                            .next()
                            .expect("control data TD must contain data TRB"),
                    ))
                } else {
                    None
                };
                let status_trb = TransferId(
                    control_addrs
                        .next()
                        .expect("control TD must contain status TRB"),
                );
                debug_assert!(
                    control_addrs.next().is_none(),
                    "control TD yielded more TRB addresses than expected"
                );
                for trb in [Some(setup_trb), data_trb, Some(status_trb)]
                    .into_iter()
                    .flatten()
                {
                    self.trb_to_request.insert(trb, request_id);
                }
                SubmittedTdKind::Control(ControlTd {
                    setup_trb,
                    data_trb,
                    status_trb,
                    requested: data_len,
                    actual: None,
                })
            }
            TransferKind::Interrupt | TransferKind::Bulk => {
                let trb = transfer::Allowed::Normal(
                    *Normal::new()
                        .set_data_buffer_pointer(data_bus_addr as _)
                        .set_trb_transfer_length(data_len as _)
                        .set_interrupter_target(0)
                        .set_interrupt_on_short_packet()
                        .set_interrupt_on_completion(),
                );
                let completion_trb = TransferId(self.ring.enque_transfer(trb));
                self.trb_to_request.insert(completion_trb, request_id);
                SubmittedTdKind::Normal { completion_trb }
            }
            TransferKind::Isochronous { packet_lengths } => {
                let packets = self.enque_iso(
                    data_bus_addr,
                    packet_lengths,
                    matches!(transfer.direction, Direction::In),
                );
                for packet in &packets {
                    self.trb_to_request.insert(packet.trb, request_id);
                }
                SubmittedTdKind::Iso { packets }
            }
        };

        self.outstanding_trbs += required_trbs;
        self.inflight.insert(
            request_id,
            SubmittedTd {
                transfer,
                kind,
                trb_count: required_trbs,
                cancelled: false,
            },
        );
        mb();
        self.doorbell();

        Ok(Self::public_request_id(request_id))
    }

    fn reclaim_request(
        &mut self,
        id: RequestId,
    ) -> Option<Result<TransferCompletion, TransferError>> {
        let request_id = Self::private_request_id(id);
        let kind = self.inflight.get(&request_id)?.kind.clone();
        match kind {
            SubmittedTdKind::Normal { completion_trb } => {
                let event = self.ring.get_finished(completion_trb.0)?;
                Some(
                    self.complete_request(request_id, completion_trb, event)
                        .map(|transfer| transfer_to_completion(id, transfer)),
                )
            }
            SubmittedTdKind::Control(control_td) => {
                self.reclaim_control_request(id, request_id, control_td)
            }
            SubmittedTdKind::Iso { .. } => self.reclaim_iso_request(id, request_id),
        }
    }

    fn register_waker(&self, id: RequestId, cx: &mut core::task::Context<'_>) {
        let request_id = Self::private_request_id(id);
        let Some(submitted) = self.inflight.get(&request_id) else {
            return;
        };
        match &submitted.kind {
            SubmittedTdKind::Normal { completion_trb } => {
                self.ring.register_cx(completion_trb.0, cx);
            }
            SubmittedTdKind::Control(control_td) => {
                control_td.register_waker(&self.ring, cx);
            }
            SubmittedTdKind::Iso { packets } => {
                for packet in packets {
                    self.ring.register_cx(packet.trb.0, cx);
                }
            }
        }
    }

    fn cancel_request(&mut self, id: RequestId) -> Result<(), TransferError> {
        let request_id = Self::private_request_id(id);
        let submitted = self
            .inflight
            .get_mut(&request_id)
            .ok_or(TransferError::InvalidEndpoint)?;
        submitted.cancelled = true;
        Ok(())
    }
}

fn iso_packet_actual_length(
    requested: usize,
    event: TransferEvent,
) -> Result<usize, TransferError> {
    let remaining = event.trb_transfer_length() as usize;
    match event.completion_code() {
        Ok(code) => match code {
            xhci::ring::trb::event::CompletionCode::Success => {
                if remaining == 0 {
                    Ok(requested)
                } else {
                    Ok(requested.saturating_sub(remaining))
                }
            }
            xhci::ring::trb::event::CompletionCode::ShortPacket
            | xhci::ring::trb::event::CompletionCode::BabbleDetectedError
            | xhci::ring::trb::event::CompletionCode::IsochBufferOverrun
            | xhci::ring::trb::event::CompletionCode::MissedServiceError
            | xhci::ring::trb::event::CompletionCode::UsbTransactionError
            | xhci::ring::trb::event::CompletionCode::Stopped => {
                Ok(requested.saturating_sub(remaining))
            }
            xhci::ring::trb::event::CompletionCode::StoppedShortPacket => Ok(remaining),
            xhci::ring::trb::event::CompletionCode::StoppedLengthInvalid => Ok(0),
            code => {
                code.to_result()?;
                Ok(requested.saturating_sub(remaining))
            }
        },
        Err(e) => Err(TransferError::Other(anyhow!(
            "unknown XHCI ISO completion code: {e:?}"
        ))),
    }
}

fn iso_packet_is_fatal(event: TransferEvent) -> bool {
    match event.completion_code() {
        Ok(
            xhci::ring::trb::event::CompletionCode::Success
            | xhci::ring::trb::event::CompletionCode::ShortPacket
            | xhci::ring::trb::event::CompletionCode::BabbleDetectedError
            | xhci::ring::trb::event::CompletionCode::IsochBufferOverrun
            | xhci::ring::trb::event::CompletionCode::MissedServiceError
            | xhci::ring::trb::event::CompletionCode::UsbTransactionError
            | xhci::ring::trb::event::CompletionCode::Stopped
            | xhci::ring::trb::event::CompletionCode::StoppedShortPacket
            | xhci::ring::trb::event::CompletionCode::StoppedLengthInvalid,
        ) => false,
        Ok(_) | Err(_) => true,
    }
}

pub(crate) trait EndpointDescriptorExt {
    fn endpoint_type(&self) -> xhci::context::EndpointType;
}

impl EndpointDescriptorExt for EndpointDescriptor {
    fn endpoint_type(&self) -> xhci::context::EndpointType {
        match self.transfer_type {
            descriptor::EndpointType::Control => xhci::context::EndpointType::Control,
            descriptor::EndpointType::Isochronous => match self.direction {
                usb_if::transfer::Direction::Out => xhci::context::EndpointType::IsochOut,
                usb_if::transfer::Direction::In => xhci::context::EndpointType::IsochIn,
            },
            descriptor::EndpointType::Bulk => match self.direction {
                usb_if::transfer::Direction::Out => xhci::context::EndpointType::BulkOut,
                usb_if::transfer::Direction::In => xhci::context::EndpointType::BulkIn,
            },
            descriptor::EndpointType::Interrupt => match self.direction {
                usb_if::transfer::Direction::Out => xhci::context::EndpointType::InterruptOut,
                usb_if::transfer::Direction::In => xhci::context::EndpointType::InterruptIn,
            },
        }
    }
}
