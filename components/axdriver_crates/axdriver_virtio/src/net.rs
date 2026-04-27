use alloc::{sync::Arc, vec::Vec};

use ax_driver_base::{BaseDriverOps, DevError, DevResult, DeviceType};
use ax_driver_net::{EthernetAddress, NetBuf, NetBufBox, NetBufPool, NetBufPtr, NetDriverOps};
use virtio_drivers::{Hal, device::net::VirtIONetRaw as InnerDev, transport::Transport};

use crate::as_dev_err;

const NET_BUF_LEN: usize = 1526;

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

fn insert_buffer<T>(slots: &mut [Option<T>], token: usize, buf: T) -> DevResult {
    let slot = slot_mut(slots, token)?;
    if slot.is_some() {
        return Err(DevError::BadState);
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

fn validate_tx_accounting<T>(slots: &[Option<T>], free_buffers: usize, queue_size: usize) -> DevResult {
    let in_flight = validate_slot_accounting(slots, queue_size)?;
    if in_flight + free_buffers != queue_size {
        return Err(DevError::BadState);
    }
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

/// The VirtIO network device driver.
///
/// `QS` is the VirtIO queue size.
pub struct VirtIoNetDev<H: Hal, T: Transport, const QS: usize> {
    rx_buffers: Vec<Option<NetBufBox>>,
    tx_buffers: Vec<Option<NetBufBox>>,
    free_tx_bufs: Vec<NetBufBox>,
    buf_pool: Arc<NetBufPool>,
    inner: InnerDev<H, T, QS>,
    irq: Option<usize>,
}

unsafe impl<H: Hal, T: Transport, const QS: usize> Send for VirtIoNetDev<H, T, QS> {}
unsafe impl<H: Hal, T: Transport, const QS: usize> Sync for VirtIoNetDev<H, T, QS> {}

impl<H: Hal, T: Transport, const QS: usize> VirtIoNetDev<H, T, QS> {
    fn queued_rx_buffers(&self) -> usize {
        count_populated_slots(&self.rx_buffers)
    }

    fn in_flight_tx_buffers(&self) -> usize {
        count_populated_slots(&self.tx_buffers)
    }

    fn has_free_tx_buffer(&self) -> bool {
        !self.free_tx_bufs.is_empty()
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

    fn fill_rx_buffers(&mut self) -> DevResult {
        for expected_token in 0..QS {
            let mut rx_buf = self.buf_pool.alloc_boxed().ok_or(DevError::NoMemory)?;
            // Safe because the buffer lives as long as the queue.
            let token = unsafe {
                self.inner
                    .receive_begin(rx_buf.raw_buf_mut())
                    .map_err(as_dev_err)?
            };
            self.validate_rx_token(token, expected_token)?;
            self.insert_rx_buffer(token as usize, rx_buf)?;
        }
        Ok(())
    }

    fn fill_tx_buffers(&mut self) -> DevResult {
        for _ in 0..QS {
            let mut tx_buf = self.buf_pool.alloc_boxed().ok_or(DevError::NoMemory)?;
            let hdr_len = self
                .inner
                .fill_buffer_header(tx_buf.raw_buf_mut())
                .map_err(as_dev_err)?;
            tx_buf.set_header_len(hdr_len);
            self.recycle_tx_buffer_box(tx_buf);
        }
        Ok(())
    }

    fn validate_rx_token(&self, token: u16, expected_token: usize) -> DevResult {
        validate_queue_token(token, expected_token, QS)
    }

    fn insert_rx_buffer(&mut self, token: usize, rx_buf: NetBufBox) -> DevResult {
        insert_buffer(&mut self.rx_buffers, token, rx_buf)
    }

    fn take_rx_buffer(&mut self, token: u16) -> DevResult<NetBufBox> {
        take_buffer(&mut self.rx_buffers, token)
    }

    fn insert_tx_buffer(&mut self, token: u16, tx_buf: NetBufBox) -> DevResult {
        insert_buffer(&mut self.tx_buffers, token as usize, tx_buf)
    }

    fn take_tx_buffer(&mut self, token: u16) -> DevResult<NetBufBox> {
        take_buffer(&mut self.tx_buffers, token)
    }

    fn validate_runtime_state(&self) -> DevResult {
        let queued_rx = self.queued_rx_buffers();
        if queued_rx > QS {
            return Err(DevError::BadState);
        }
        let in_flight_tx = self.in_flight_tx_buffers();
        if in_flight_tx > QS {
            return Err(DevError::BadState);
        }
        validate_slot_accounting(&self.rx_buffers, QS)?;
        validate_tx_accounting(&self.tx_buffers, self.free_tx_bufs.len(), QS)?;
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
        let free_tx_bufs = Vec::with_capacity(QS);

        let mut dev = Self {
            rx_buffers,
            inner,
            tx_buffers,
            free_tx_bufs,
            buf_pool,
            irq,
        };

        // 1. Fill all rx buffers.
        dev.fill_rx_buffers()?;

        // 2. Allocate all tx buffers.
        dev.fill_tx_buffers()?;

        // 3. Validate queue bookkeeping before exposing the device.
        dev.validate_runtime_state()?;

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
        self.has_free_tx_buffer() && self.inner.can_send()
    }

    #[inline]
    fn can_receive(&self) -> bool {
        self.inner.poll_receive().is_some()
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
        let mut rx_buf = unsafe { NetBuf::from_buf_ptr(rx_buf) };
        // Safe because we take the ownership of `rx_buf` back to `rx_buffers`,
        // it lives as long as the queue.
        let new_token = unsafe {
            self.inner
                .receive_begin(rx_buf.raw_buf_mut())
                .map_err(as_dev_err)?
        };
        self.insert_rx_buffer(new_token as usize, rx_buf)?;
        self.validate_runtime_state()
    }

    fn recycle_tx_buffers(&mut self) -> DevResult {
        self.validate_runtime_state()?;
        while let Some(token) = self.inner.poll_transmit() {
            let tx_buf = self.take_tx_buffer(token)?;
            unsafe {
                self.inner
                    .transmit_complete(token, tx_buf.packet_with_header())
                    .map_err(as_dev_err)?;
            }
            // Recycle the buffer.
            self.recycle_tx_buffer_box(tx_buf);
        }
        self.validate_runtime_state()
    }

    fn transmit(&mut self, tx_buf: NetBufPtr) -> DevResult {
        self.validate_runtime_state()?;
        // 0. prepare tx buffer.
        let tx_buf = unsafe { NetBuf::from_buf_ptr(tx_buf) };
        // 1. transmit packet.
        let token = unsafe {
            self.inner
                .transmit_begin(tx_buf.packet_with_header())
                .map_err(as_dev_err)?
        };
        self.insert_tx_buffer(token, tx_buf)?;
        self.validate_runtime_state()
    }

    fn receive(&mut self) -> DevResult<NetBufPtr> {
        self.validate_runtime_state()?;
        self.inner.ack_interrupt();
        if let Some(token) = self.inner.poll_receive() {
            let mut rx_buf = self.take_rx_buffer(token)?;
            // Safe because the buffer lives as long as the queue.
            let (hdr_len, pkt_len) = unsafe {
                self.inner
                    .receive_complete(token, rx_buf.raw_buf_mut())
                    .map_err(as_dev_err)?
            };
            rx_buf.set_header_len(hdr_len);
            rx_buf.set_packet_len(pkt_len);

            self.validate_runtime_state()?;
            Ok(rx_buf.into_buf_ptr())
        } else {
            Err(DevError::Again)
        }
    }

    fn alloc_tx_buffer(&mut self, size: usize) -> DevResult<NetBufPtr> {
        self.validate_runtime_state()?;
        // 0. Allocate a buffer from the queue.
        let mut net_buf = self.alloc_tx_buffer_box()?;

        // 1. Check if the buffer is large enough.
        self.prepare_tx_buffer(&mut net_buf, size)?;

        // 2. Return the buffer.
        self.validate_runtime_state()?;
        Ok(net_buf.into_buf_ptr())
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use ax_driver_base::DevError;

    use super::{
        count_populated_slots, insert_buffer, take_buffer, validate_packet_layout,
        validate_queue_token, validate_slot_accounting, validate_tx_accounting,
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
        assert!(matches!(take_buffer(&mut slots, 0), Err(DevError::BadState)));
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
        assert!(validate_tx_accounting(&slots, 2, 4).is_ok());
    }

    #[test]
    fn validate_tx_accounting_rejects_unbalanced_state() {
        let slots = vec![Some(1u8), None, Some(3u8), None];
        assert!(matches!(
            validate_tx_accounting(&slots, 1, 4),
            Err(DevError::BadState)
        ));
    }
}
