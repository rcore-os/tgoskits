use alloc::{sync::Arc, vec::Vec};

use ax_driver_base::{BaseDriverOps, DevError, DevResult, DeviceType};
use ax_driver_net::{
    EthernetAddress, NetBuf, NetBufBox, NetBufPool, NetBufPtr, NetDriverOps, NetIrqEvent,
};
use virtio_drivers::{Hal, device::net::VirtIONetRaw as InnerDev, transport::Transport};

use crate::as_dev_err;

const NET_BUF_LEN: usize = 1526;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct QueueOccupancy {
    occupied: usize,
    capacity: usize,
}

impl QueueOccupancy {
    const fn new(occupied: usize, capacity: usize) -> Self {
        Self { occupied, capacity }
    }

    const fn vacant(self) -> usize {
        self.capacity - self.occupied
    }

    const fn has_capacity_for(self, needed: usize) -> bool {
        self.vacant() >= needed
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RuntimeStateSnapshot {
    rx: QueueOccupancy,
    tx_in_flight: QueueOccupancy,
    free_rx_buffers: usize,
    free_tx_buffers: usize,
    checked_out_rx_buffers: usize,
    checked_out_tx_buffers: usize,
}

impl RuntimeStateSnapshot {
    const fn queue_size(self) -> usize {
        self.rx.capacity
    }

    const fn provisioned_tx_buffers(self) -> usize {
        self.tx_in_flight.occupied + self.free_tx_buffers + self.checked_out_tx_buffers
    }

    const fn all_rx_buffers_accounted_for(self) -> bool {
        self.rx.occupied + self.free_rx_buffers + self.checked_out_rx_buffers == self.rx.capacity
    }

    const fn all_tx_buffers_accounted_for(self) -> bool {
        self.provisioned_tx_buffers() == self.tx_in_flight.capacity
    }

    const fn can_allocate_tx(self) -> bool {
        self.free_tx_buffers != 0
    }

    const fn can_provision_tx(self, needed: usize) -> bool {
        self.queue_size() - self.provisioned_tx_buffers() >= needed
    }
}

fn validate_queue_token(token: u16, expected_token: usize, queue_size: usize) -> DevResult {
    let token = token as usize;
    if token >= queue_size {
        return Err(DevError::BadState);
    }
    if token != expected_token {
        return Err(DevError::BadState);
    }
    Ok(())
}

fn slot_mut<T>(slots: &mut [Option<T>], token: usize) -> DevResult<&mut Option<T>> {
    slots.get_mut(token).ok_or(DevError::BadState)
}

#[cfg(test)]
fn insert_buffer<T>(slots: &mut [Option<T>], token: usize, buf: T) -> DevResult {
    let slot = slot_mut(slots, token)?;
    if slot.is_some() {
        return Err(DevError::BadState);
    }
    *slot = Some(buf);
    Ok(())
}

fn insert_buffer_or_return<T>(slots: &mut [Option<T>], token: usize, buf: T) -> Result<(), T> {
    let Some(slot) = slots.get_mut(token) else {
        return Err(buf);
    };
    if slot.is_some() {
        return Err(buf);
    }
    *slot = Some(buf);
    Ok(())
}

fn take_buffer<T>(slots: &mut [Option<T>], token: u16) -> DevResult<T> {
    slot_mut(slots, token as usize)?
        .take()
        .ok_or(DevError::BadState)
}

fn count_populated_slots<T>(slots: &[Option<T>]) -> usize {
    slots.iter().filter(|slot| slot.is_some()).count()
}

fn validate_slot_accounting<T>(slots: &[Option<T>], queue_size: usize) -> DevResult<usize> {
    let occupied = count_populated_slots(slots);
    if occupied > queue_size {
        return Err(DevError::BadState);
    }
    Ok(occupied)
}

fn validate_tx_accounting<T>(
    slots: &[Option<T>],
    free_buffers: usize,
    checked_out_buffers: usize,
    queue_size: usize,
) -> DevResult {
    let in_flight = validate_slot_accounting(slots, queue_size)?;
    if in_flight + free_buffers + checked_out_buffers != queue_size {
        return Err(DevError::BadState);
    }
    Ok(())
}

fn rollback_checked_out_tx_buffer<T>(
    free_buffers: &mut Vec<T>,
    checked_out_buffers: &mut usize,
    tx_buf: T,
) -> DevResult {
    if *checked_out_buffers == 0 {
        return Err(DevError::BadState);
    }
    *checked_out_buffers -= 1;
    free_buffers.push(tx_buf);
    Ok(())
}

fn rollback_checked_out_rx_buffer<T>(
    free_buffers: &mut Vec<T>,
    checked_out_buffers: &mut usize,
    rx_buf: T,
) -> DevResult {
    if *checked_out_buffers == 0 {
        return Err(DevError::BadState);
    }
    *checked_out_buffers -= 1;
    free_buffers.push(rx_buf);
    Ok(())
}

fn validate_packet_layout(header_len: usize, packet_len: usize, capacity: usize) -> DevResult {
    if header_len > capacity {
        return Err(DevError::BadState);
    }
    if header_len + packet_len > capacity {
        return Err(DevError::InvalidParam);
    }
    Ok(())
}

fn validate_received_packet_layout(
    header_len: usize,
    packet_len: usize,
    capacity: usize,
) -> DevResult {
    validate_packet_layout(header_len, packet_len, capacity)
}

fn queue_occupancy<T>(slots: &[Option<T>], queue_size: usize) -> DevResult<QueueOccupancy> {
    let occupied = validate_slot_accounting(slots, queue_size)?;
    Ok(QueueOccupancy::new(occupied, queue_size))
}

fn validate_runtime_snapshot(snapshot: RuntimeStateSnapshot) -> DevResult {
    if snapshot.rx.capacity != snapshot.tx_in_flight.capacity {
        return Err(DevError::BadState);
    }
    if !snapshot.all_rx_buffers_accounted_for() {
        return Err(DevError::BadState);
    }
    if !snapshot.all_tx_buffers_accounted_for() {
        return Err(DevError::BadState);
    }
    Ok(())
}

/// The VirtIO network device driver.
///
/// `QS` is the VirtIO queue size.
pub struct VirtIoNetDev<H: Hal, T: Transport, const QS: usize> {
    rx_buffers: Vec<Option<NetBufBox>>,
    tx_buffers: Vec<Option<NetBufBox>>,
    free_rx_bufs: Vec<NetBufBox>,
    free_tx_bufs: Vec<NetBufBox>,
    checked_out_rx_buffers: usize,
    checked_out_tx_buffers: usize,
    poisoned: bool,
    buf_pool: Arc<NetBufPool>,
    inner: InnerDev<H, T, QS>,
    irq: Option<usize>,
}

unsafe impl<H: Hal, T: Transport, const QS: usize> Send for VirtIoNetDev<H, T, QS> {}
unsafe impl<H: Hal, T: Transport, const QS: usize> Sync for VirtIoNetDev<H, T, QS> {}

impl<H: Hal, T: Transport, const QS: usize> VirtIoNetDev<H, T, QS> {
    fn ensure_not_poisoned(&self) -> DevResult {
        if self.poisoned {
            return Err(DevError::BadState);
        }
        Ok(())
    }

    fn queue_capacity(&self) -> usize {
        QS
    }

    fn queued_rx_buffers(&self) -> usize {
        count_populated_slots(&self.rx_buffers)
    }

    fn in_flight_tx_buffers(&self) -> usize {
        count_populated_slots(&self.tx_buffers)
    }

    fn free_tx_buffer_count(&self) -> usize {
        self.free_tx_bufs.len()
    }

    fn free_rx_buffer_count(&self) -> usize {
        self.free_rx_bufs.len()
    }

    fn checked_out_rx_buffer_count(&self) -> usize {
        self.checked_out_rx_buffers
    }

    fn checked_out_tx_buffer_count(&self) -> usize {
        self.checked_out_tx_buffers
    }

    fn has_queued_rx_buffer(&self) -> bool {
        self.runtime_state_snapshot()
            .map(|snapshot| snapshot.rx.occupied != 0)
            .unwrap_or(false)
    }

    fn has_in_flight_tx_buffer(&self) -> bool {
        self.runtime_state_snapshot()
            .map(|snapshot| snapshot.tx_in_flight.occupied != 0)
            .unwrap_or(false)
    }

    fn has_free_tx_buffer(&self) -> bool {
        self.runtime_state_snapshot()
            .map(|snapshot| snapshot.can_allocate_tx())
            .unwrap_or(false)
    }

    fn checkout_rx_buffer(&mut self) -> DevResult {
        let checked_out = self.checked_out_rx_buffer_count();
        if checked_out >= self.queue_capacity() {
            return Err(DevError::BadState);
        }
        self.checked_out_rx_buffers += 1;
        Ok(())
    }

    fn recycle_checked_out_rx_buffer(&mut self) -> DevResult {
        if self.checked_out_rx_buffers == 0 {
            return Err(DevError::BadState);
        }
        self.checked_out_rx_buffers -= 1;
        Ok(())
    }

    fn checkout_tx_buffer(&mut self) -> DevResult {
        let checked_out = self.checked_out_tx_buffer_count();
        if checked_out >= self.queue_capacity() {
            return Err(DevError::BadState);
        }
        self.checked_out_tx_buffers += 1;
        Ok(())
    }

    fn submit_checked_out_tx_buffer(&mut self) -> DevResult {
        if self.checked_out_tx_buffers == 0 {
            return Err(DevError::BadState);
        }
        self.checked_out_tx_buffers -= 1;
        Ok(())
    }

    fn recycle_tx_buffer_box(&mut self, tx_buf: NetBufBox) {
        self.free_tx_bufs.push(tx_buf);
    }

    fn alloc_tx_buffer_box(&mut self) -> DevResult<NetBufBox> {
        self.free_tx_bufs.pop().ok_or(DevError::NoMemory)
    }

    fn prepare_tx_buffer(&self, tx_buf: &mut NetBuf, packet_len: usize) -> DevResult {
        let header_len = tx_buf.header_len();
        validate_packet_layout(header_len, packet_len, tx_buf.capacity())?;
        tx_buf.set_packet_len(packet_len);
        Ok(())
    }

    fn runtime_state_snapshot(&self) -> DevResult<RuntimeStateSnapshot> {
        self.ensure_not_poisoned()?;
        let rx = queue_occupancy(&self.rx_buffers, self.queue_capacity())?;
        let tx_in_flight = queue_occupancy(&self.tx_buffers, self.queue_capacity())?;
        Ok(RuntimeStateSnapshot {
            rx,
            tx_in_flight,
            free_rx_buffers: self.free_rx_buffer_count(),
            free_tx_buffers: self.free_tx_buffer_count(),
            checked_out_rx_buffers: self.checked_out_rx_buffer_count(),
            checked_out_tx_buffers: self.checked_out_tx_buffer_count(),
        })
    }

    fn validate_tx_capacity(&self, needed: usize) -> DevResult {
        let snapshot = self.runtime_state_snapshot()?;
        if snapshot.free_tx_buffers < needed {
            return Err(DevError::NoMemory);
        }
        Ok(())
    }

    fn validate_tx_provision_capacity(&self, needed: usize) -> DevResult {
        let snapshot = self.runtime_state_snapshot()?;
        if !snapshot.can_provision_tx(needed) {
            return Err(DevError::BadState);
        }
        Ok(())
    }

    fn validate_rx_capacity(&self, needed: usize) -> DevResult {
        let snapshot = self.runtime_state_snapshot()?;
        if !snapshot.rx.has_capacity_for(needed) {
            return Err(DevError::BadState);
        }
        Ok(())
    }

    fn validate_tx_completion_capacity(&self) -> DevResult {
        let snapshot = self.runtime_state_snapshot()?;
        if snapshot.tx_in_flight.occupied > snapshot.queue_size() {
            return Err(DevError::BadState);
        }
        Ok(())
    }

    fn validate_before_rx_recycle(&self) -> DevResult {
        let snapshot = self.runtime_state_snapshot()?;
        if !snapshot.rx.has_capacity_for(1) {
            return Err(DevError::BadState);
        }
        if snapshot.checked_out_rx_buffers == 0 {
            return Err(DevError::BadState);
        }
        Ok(())
    }

    fn validate_before_tx_recycle(&self) -> DevResult {
        if !self.has_in_flight_tx_buffer() {
            return Ok(());
        }
        self.validate_tx_completion_capacity()
    }

    fn validate_before_transmit(&self) -> DevResult {
        if self.checked_out_tx_buffer_count() == 0 {
            return Err(DevError::BadState);
        }
        Ok(())
    }

    fn validate_before_receive(&self) -> DevResult {
        if !self.has_queued_rx_buffer() {
            return Err(DevError::BadState);
        }
        Ok(())
    }

    fn allocate_pool_buffer(&self) -> DevResult<NetBufBox> {
        self.buf_pool.alloc_boxed().ok_or(DevError::NoMemory)
    }

    fn poison(&mut self) {
        self.poisoned = true;
    }

    fn poison_submitted_rx_buffer(&mut self, rx_buf: NetBufBox) -> DevError {
        let _ = self.recycle_checked_out_rx_buffer();
        self.poison();
        core::mem::forget(rx_buf);
        DevError::BadState
    }

    fn rollback_unsubmitted_rx_buffer(&mut self, rx_buf: NetBufBox, err: DevError) -> DevError {
        if rollback_checked_out_rx_buffer(
            &mut self.free_rx_bufs,
            &mut self.checked_out_rx_buffers,
            rx_buf,
        )
        .is_err()
        {
            self.poison();
            return DevError::BadState;
        }
        err
    }

    fn poison_submitted_tx_buffer(&mut self, tx_buf: NetBufBox) -> DevError {
        let _ = self.submit_checked_out_tx_buffer();
        self.poison();
        core::mem::forget(tx_buf);
        DevError::BadState
    }

    fn rollback_unsubmitted_tx_buffer(&mut self, tx_buf: NetBufBox, err: DevError) -> DevError {
        if rollback_checked_out_tx_buffer(
            &mut self.free_tx_bufs,
            &mut self.checked_out_tx_buffers,
            tx_buf,
        )
        .is_err()
        {
            self.poison();
            return DevError::BadState;
        }
        err
    }

    fn prime_rx_buffer(&mut self, expected_token: usize) -> DevResult {
        self.validate_rx_capacity(1)?;
        let mut rx_buf = self.allocate_pool_buffer()?;
        let token = unsafe {
            self.inner
                .receive_begin(rx_buf.raw_buf_mut())
                .map_err(as_dev_err)?
        };
        if self.validate_rx_token(token, expected_token).is_err() {
            self.poison();
            core::mem::forget(rx_buf);
            return Err(DevError::BadState);
        }
        if let Err(rx_buf) = self.insert_rx_buffer_or_return(token as usize, rx_buf) {
            self.poison();
            core::mem::forget(rx_buf);
            return Err(DevError::BadState);
        }
        Ok(())
    }

    fn fill_rx_buffers(&mut self) -> DevResult {
        for expected_token in 0..QS {
            self.prime_rx_buffer(expected_token)?;
        }
        Ok(())
    }

    fn prepare_free_tx_buffer(&mut self) -> DevResult {
        self.validate_tx_provision_capacity(1)?;
        let mut tx_buf = self.allocate_pool_buffer()?;
        let hdr_len = self
            .inner
            .fill_buffer_header(tx_buf.raw_buf_mut())
            .map_err(as_dev_err)?;
        tx_buf.set_header_len(hdr_len);
        self.recycle_tx_buffer_box(tx_buf);
        Ok(())
    }

    fn fill_tx_buffers(&mut self) -> DevResult {
        for _ in 0..QS {
            self.prepare_free_tx_buffer()?;
        }
        Ok(())
    }

    fn validate_rx_token(&self, token: u16, expected_token: usize) -> DevResult {
        validate_queue_token(token, expected_token, QS)
    }

    fn insert_rx_buffer_or_return(
        &mut self,
        token: usize,
        rx_buf: NetBufBox,
    ) -> Result<(), NetBufBox> {
        insert_buffer_or_return(&mut self.rx_buffers, token, rx_buf)
    }

    fn take_rx_buffer(&mut self, token: u16) -> DevResult<NetBufBox> {
        take_buffer(&mut self.rx_buffers, token)
    }

    fn insert_tx_buffer_or_return(
        &mut self,
        token: u16,
        tx_buf: NetBufBox,
    ) -> Result<(), NetBufBox> {
        insert_buffer_or_return(&mut self.tx_buffers, token as usize, tx_buf)
    }

    fn take_tx_buffer(&mut self, token: u16) -> DevResult<NetBufBox> {
        take_buffer(&mut self.tx_buffers, token)
    }

    fn begin_recycled_rx_buffer(&mut self, rx_buf: &mut NetBuf) -> DevResult<u16> {
        unsafe {
            self.inner
                .receive_begin(rx_buf.raw_buf_mut())
                .map_err(as_dev_err)
        }
    }

    fn replenish_free_rx_buffers(&mut self) -> DevResult {
        while let Some(mut rx_buf) = self.free_rx_bufs.pop() {
            match self.begin_recycled_rx_buffer(&mut rx_buf) {
                Ok(token) => {
                    if let Err(rx_buf) = self.insert_rx_buffer_or_return(token as usize, rx_buf) {
                        return Err(self.poison_submitted_rx_buffer(rx_buf));
                    }
                }
                Err(err) => {
                    self.free_rx_bufs.push(rx_buf);
                    return Err(err);
                }
            }
        }
        Ok(())
    }

    fn submit_recycled_rx_buffer(&mut self, rx_buf: NetBufBox) -> DevResult {
        self.validate_rx_capacity(1)?;
        let mut rx_buf = rx_buf;
        let token = match self.begin_recycled_rx_buffer(&mut rx_buf) {
            Ok(token) => token,
            Err(err) => return Err(self.rollback_unsubmitted_rx_buffer(rx_buf, err)),
        };
        if let Err(rx_buf) = self.insert_rx_buffer_or_return(token as usize, rx_buf) {
            return Err(self.poison_submitted_rx_buffer(rx_buf));
        }
        self.recycle_checked_out_rx_buffer()
    }

    fn poll_completed_tx_token(&mut self) -> Option<u16> {
        self.inner.poll_transmit()
    }

    fn complete_tx_buffer(&mut self, token: u16) -> DevResult<NetBufBox> {
        self.validate_tx_completion_capacity()?;
        let tx_buf = self.take_tx_buffer(token)?;
        unsafe {
            self.inner
                .transmit_complete(token, tx_buf.packet_with_header())
                .map_err(as_dev_err)?;
        }
        Ok(tx_buf)
    }

    fn recycle_completed_transmissions(&mut self) -> DevResult<usize> {
        let mut recycled = 0;
        while let Some(token) = self.poll_completed_tx_token() {
            let tx_buf = self.complete_tx_buffer(token)?;
            self.recycle_tx_buffer_box(tx_buf);
            recycled += 1;
        }
        Ok(recycled)
    }

    fn submit_tx_buffer(&mut self, tx_buf: NetBufBox) -> DevResult {
        let token = match unsafe { self.inner.transmit_begin(tx_buf.packet_with_header()) } {
            Ok(token) => token,
            Err(err) => return Err(self.rollback_unsubmitted_tx_buffer(tx_buf, as_dev_err(err))),
        };
        if let Err(tx_buf) = self.insert_tx_buffer_or_return(token, tx_buf) {
            return Err(self.poison_submitted_tx_buffer(tx_buf));
        }
        self.submit_checked_out_tx_buffer()
    }

    fn poll_received_token(&mut self) -> Option<u16> {
        self.inner.poll_receive()
    }

    fn complete_received_buffer(&mut self, token: u16) -> DevResult<NetBufBox> {
        let mut rx_buf = self.take_rx_buffer(token)?;
        let (hdr_len, pkt_len) = unsafe {
            self.inner
                .receive_complete(token, rx_buf.raw_buf_mut())
                .map_err(as_dev_err)?
        };
        if let Err(err) = validate_received_packet_layout(hdr_len, pkt_len, rx_buf.capacity()) {
            self.free_rx_bufs.push(rx_buf);
            let _ = self.replenish_free_rx_buffers();
            return Err(err);
        }
        rx_buf.set_header_len(hdr_len);
        rx_buf.set_packet_len(pkt_len);
        Ok(rx_buf)
    }

    fn validate_runtime_state(&self) -> DevResult {
        let snapshot = self.runtime_state_snapshot()?;
        if self.queued_rx_buffers() != snapshot.rx.occupied {
            return Err(DevError::BadState);
        }
        if self.in_flight_tx_buffers() != snapshot.tx_in_flight.occupied {
            return Err(DevError::BadState);
        }
        validate_tx_accounting(
            &self.tx_buffers,
            self.free_tx_buffer_count(),
            self.checked_out_tx_buffer_count(),
            QS,
        )?;
        validate_runtime_snapshot(snapshot)?;
        Ok(())
    }

    /// Creates a new driver instance and initializes the device, or returns
    /// an error if any step fails.
    pub fn try_new(transport: T, irq: Option<usize>) -> DevResult<Self> {
        // Keep queue bookkeeping on the heap to avoid very large debug stack frames.
        let inner = InnerDev::new(transport).map_err(as_dev_err)?;
        let mut rx_buffers = Vec::with_capacity(QS);
        rx_buffers.resize_with(QS, || None);
        let mut tx_buffers = Vec::with_capacity(QS);
        tx_buffers.resize_with(QS, || None);
        let buf_pool = NetBufPool::new(2 * QS, NET_BUF_LEN)?;
        let free_rx_bufs = Vec::with_capacity(QS);
        let free_tx_bufs = Vec::with_capacity(QS);

        let mut dev = Self {
            rx_buffers,
            inner,
            tx_buffers,
            free_rx_bufs,
            free_tx_bufs,
            checked_out_rx_buffers: 0,
            checked_out_tx_buffers: 0,
            poisoned: false,
            buf_pool,
            irq,
        };

        // 1. Fill all rx buffers.
        dev.fill_rx_buffers()?;

        // 2. Allocate all tx buffers.
        dev.fill_tx_buffers()?;

        // 3. Validate queue bookkeeping before exposing the device.
        dev.validate_runtime_state()?;

        if irq.is_some() {
            dev.inner.enable_interrupts();
        }

        // 4. Return the driver instance.
        Ok(dev)
    }
}

impl<H: Hal, T: Transport, const QS: usize> BaseDriverOps for VirtIoNetDev<H, T, QS> {
    fn device_name(&self) -> &str {
        "virtio-net"
    }

    fn device_type(&self) -> DeviceType {
        DeviceType::Net
    }

    fn irq_num(&self) -> Option<usize> {
        self.irq
    }
}

impl<H: Hal, T: Transport, const QS: usize> NetDriverOps for VirtIoNetDev<H, T, QS> {
    #[inline]
    fn mac_address(&self) -> EthernetAddress {
        EthernetAddress(self.inner.mac_address())
    }

    #[inline]
    fn can_transmit(&self) -> bool {
        !self.poisoned && self.has_free_tx_buffer() && self.inner.can_send()
    }

    #[inline]
    fn can_receive(&self) -> bool {
        !self.poisoned && self.inner.poll_receive().is_some()
    }

    #[inline]
    fn rx_queue_size(&self) -> usize {
        QS
    }

    #[inline]
    fn tx_queue_size(&self) -> usize {
        QS
    }

    fn recycle_rx_buffer(&mut self, rx_buf: NetBufPtr) -> DevResult {
        self.validate_runtime_state()?;
        self.validate_before_rx_recycle()?;
        let rx_buf = unsafe { NetBuf::from_buf_ptr(rx_buf) };
        self.submit_recycled_rx_buffer(rx_buf)?;
        let _ = self.replenish_free_rx_buffers();
        self.validate_runtime_state()
    }

    fn recycle_tx_buffers(&mut self) -> DevResult {
        self.validate_runtime_state()?;
        self.validate_before_tx_recycle()?;
        let _ = self.recycle_completed_transmissions()?;
        self.validate_runtime_state()
    }

    fn transmit(&mut self, tx_buf: NetBufPtr) -> DevResult {
        self.validate_runtime_state()?;
        self.validate_before_transmit()?;
        let tx_buf = unsafe { NetBuf::from_buf_ptr(tx_buf) };
        self.submit_tx_buffer(tx_buf)?;
        self.validate_runtime_state()
    }

    fn receive(&mut self) -> DevResult<NetBufPtr> {
        self.validate_runtime_state()?;
        self.validate_before_receive()?;
        if let Some(token) = self.poll_received_token() {
            let rx_buf = self.complete_received_buffer(token)?;
            self.checkout_rx_buffer()?;
            self.validate_runtime_state()?;
            Ok(rx_buf.into_buf_ptr())
        } else {
            Err(DevError::Again)
        }
    }

    fn alloc_tx_buffer(&mut self, size: usize) -> DevResult<NetBufPtr> {
        self.validate_runtime_state()?;
        self.validate_tx_capacity(1)?;
        // 0. Allocate a buffer from the queue.
        let mut net_buf = self.alloc_tx_buffer_box()?;
        self.checkout_tx_buffer()?;

        // 1. Check if the buffer is large enough.
        if let Err(err) = self.prepare_tx_buffer(&mut net_buf, size) {
            self.recycle_tx_buffer_box(net_buf);
            self.submit_checked_out_tx_buffer()?;
            return Err(err);
        }

        // 2. Return the buffer.
        self.validate_runtime_state()?;
        Ok(net_buf.into_buf_ptr())
    }

    fn handle_irq(&mut self) -> NetIrqEvent {
        self.inner.ack_interrupt();

        let mut events = NetIrqEvent::empty();
        if self.inner.poll_receive().is_some() {
            events |= NetIrqEvent::RX_READY;
        }

        if self
            .recycle_completed_transmissions()
            .ok()
            .is_some_and(|count| count != 0)
        {
            events |= NetIrqEvent::TX_DONE;
        }

        if events.is_empty() {
            NetIrqEvent::SPURIOUS
        } else {
            events
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use ax_driver_base::DevError;

    use super::{
        QueueOccupancy, RuntimeStateSnapshot, count_populated_slots, insert_buffer,
        rollback_checked_out_rx_buffer, rollback_checked_out_tx_buffer, take_buffer,
        validate_packet_layout, validate_queue_token, validate_received_packet_layout,
        validate_runtime_snapshot, validate_slot_accounting, validate_tx_accounting,
    };

    #[test]
    fn validate_queue_token_accepts_expected_token() {
        assert!(validate_queue_token(2, 2, 4).is_ok());
    }

    #[test]
    fn validate_queue_token_rejects_out_of_range_token() {
        assert!(matches!(
            validate_queue_token(4, 4, 4),
            Err(DevError::BadState)
        ));
    }

    #[test]
    fn validate_queue_token_rejects_unexpected_token() {
        assert!(matches!(
            validate_queue_token(1, 2, 4),
            Err(DevError::BadState)
        ));
    }

    #[test]
    fn insert_buffer_rejects_duplicate_slot() {
        let mut slots = vec![Some(1u8), None];
        assert!(matches!(
            insert_buffer(&mut slots, 0, 2u8),
            Err(DevError::BadState)
        ));
    }

    #[test]
    fn take_buffer_rejects_empty_slot() {
        let mut slots = vec![None::<u8>, Some(2u8)];
        assert!(matches!(
            take_buffer(&mut slots, 0),
            Err(DevError::BadState)
        ));
    }

    #[test]
    fn insert_and_take_buffer_round_trip() {
        let mut slots = vec![None::<u8>, None];
        insert_buffer(&mut slots, 1, 7u8).unwrap();
        let value = take_buffer(&mut slots, 1).unwrap();
        assert_eq!(value, 7);
        assert!(slots[1].is_none());
    }

    #[test]
    fn validate_packet_layout_accepts_valid_lengths() {
        assert!(validate_packet_layout(14, 128, 1526).is_ok());
    }

    #[test]
    fn validate_packet_layout_rejects_oversized_packet() {
        assert!(matches!(
            validate_packet_layout(64, 1500, 1526),
            Err(DevError::InvalidParam)
        ));
    }

    #[test]
    fn validate_packet_layout_rejects_invalid_header_len() {
        assert!(matches!(
            validate_packet_layout(1600, 8, 1526),
            Err(DevError::BadState)
        ));
    }

    #[test]
    fn validate_received_packet_layout_accepts_capacity_boundary() {
        assert!(validate_received_packet_layout(10, 1516, 1526).is_ok());
    }

    #[test]
    fn validate_received_packet_layout_rejects_oversized_total_len() {
        assert!(matches!(
            validate_received_packet_layout(14, 1513, 1526),
            Err(DevError::InvalidParam)
        ));
    }

    #[test]
    fn count_populated_slots_counts_present_entries() {
        let slots = vec![Some(1u8), None, Some(3u8)];
        assert_eq!(count_populated_slots(&slots), 2);
    }

    #[test]
    fn validate_slot_accounting_accepts_valid_occupancy() {
        let slots = vec![Some(1u8), None, Some(3u8)];
        assert_eq!(validate_slot_accounting(&slots, 3).unwrap(), 2);
    }

    #[test]
    fn validate_tx_accounting_accepts_balanced_state() {
        let slots = vec![Some(1u8), None, Some(3u8), None];
        assert!(validate_tx_accounting(&slots, 2, 0, 4).is_ok());
    }

    #[test]
    fn validate_tx_accounting_rejects_unbalanced_state() {
        let slots = vec![Some(1u8), None, Some(3u8), None];
        assert!(matches!(
            validate_tx_accounting(&slots, 1, 0, 4),
            Err(DevError::BadState)
        ));
    }

    #[test]
    fn rollback_checked_out_tx_buffer_restores_free_list_and_count() {
        let mut free_buffers = vec![1u8];
        let mut checked_out_buffers = 1usize;
        rollback_checked_out_tx_buffer(&mut free_buffers, &mut checked_out_buffers, 2u8).unwrap();
        assert_eq!(checked_out_buffers, 0);
        assert_eq!(free_buffers, vec![1u8, 2u8]);
    }

    #[test]
    fn rollback_checked_out_tx_buffer_rejects_missing_checked_out_state() {
        let mut free_buffers = vec![1u8];
        let mut checked_out_buffers = 0usize;
        assert!(matches!(
            rollback_checked_out_tx_buffer(&mut free_buffers, &mut checked_out_buffers, 2u8),
            Err(DevError::BadState)
        ));
        assert_eq!(checked_out_buffers, 0);
        assert_eq!(free_buffers, vec![1u8]);
    }

    #[test]
    fn rollback_checked_out_rx_buffer_restores_free_list_and_count() {
        let mut free_buffers = vec![1u8];
        let mut checked_out_buffers = 1usize;
        rollback_checked_out_rx_buffer(&mut free_buffers, &mut checked_out_buffers, 2u8).unwrap();
        assert_eq!(checked_out_buffers, 0);
        assert_eq!(free_buffers, vec![1u8, 2u8]);
    }

    #[test]
    fn rollback_checked_out_rx_buffer_rejects_missing_checked_out_state() {
        let mut free_buffers = vec![1u8];
        let mut checked_out_buffers = 0usize;
        assert!(matches!(
            rollback_checked_out_rx_buffer(&mut free_buffers, &mut checked_out_buffers, 2u8),
            Err(DevError::BadState)
        ));
        assert_eq!(checked_out_buffers, 0);
        assert_eq!(free_buffers, vec![1u8]);
    }

    #[test]
    fn runtime_snapshot_accepts_tx_provisioning_before_free_list_is_filled() {
        let snapshot = RuntimeStateSnapshot {
            rx: QueueOccupancy::new(4, 4),
            tx_in_flight: QueueOccupancy::new(0, 4),
            free_rx_buffers: 0,
            free_tx_buffers: 0,
            checked_out_rx_buffers: 0,
            checked_out_tx_buffers: 0,
        };
        assert!(snapshot.can_provision_tx(1));
        assert!(!snapshot.can_allocate_tx());
    }

    #[test]
    fn validate_runtime_snapshot_accepts_reclaimed_rx_buffer() {
        let snapshot = RuntimeStateSnapshot {
            rx: QueueOccupancy::new(3, 4),
            tx_in_flight: QueueOccupancy::new(0, 4),
            free_rx_buffers: 1,
            free_tx_buffers: 4,
            checked_out_rx_buffers: 0,
            checked_out_tx_buffers: 0,
        };

        assert!(validate_runtime_snapshot(snapshot).is_ok());
    }

    #[test]
    fn validate_runtime_snapshot_rejects_lost_rx_buffer() {
        let snapshot = RuntimeStateSnapshot {
            rx: QueueOccupancy::new(3, 4),
            tx_in_flight: QueueOccupancy::new(0, 4),
            free_rx_buffers: 0,
            free_tx_buffers: 4,
            checked_out_rx_buffers: 0,
            checked_out_tx_buffers: 0,
        };

        assert!(matches!(
            validate_runtime_snapshot(snapshot),
            Err(DevError::BadState)
        ));
    }
}
