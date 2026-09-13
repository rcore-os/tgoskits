//! On-demand file I/O with one bounded, owned request in flight.
//!
//! The virtqueue retries its pending head after `WouldBlock`. Only the worker
//! touches storage; guest memory never escapes the scoped polling call.
//! Reset discards queued/completed work and drains running I/O before reuse.

use std::{
    format,
    sync::{Arc, Mutex},
    thread::JoinHandle,
    vec::Vec,
};

use axvirtio_blk::{BlockBackend, VirtioBlockConfig};
use axvirtio_common::{VirtioError, VirtioResult};

fn max_request_bytes() -> usize {
    let config = VirtioBlockConfig::default();
    config.seg_max as usize * config.size_max as usize
}
const IO_CHUNK_BYTES: usize = 4096;
const WORKER_STACK_BYTES: usize = 1024 * 1024;

pub(super) struct FileBackend {
    shared: Arc<Mutex<Shared>>,
    worker: Option<JoinHandle<()>>,
    pub(super) capacity_sectors: u64,
}

trait Storage: Send + 'static {
    fn read_at(&mut self, offset: u64, bytes: &mut [u8]) -> VirtioResult<usize>;
    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> VirtioResult<usize>;
    fn sync(&mut self) -> VirtioResult<()>;
}

struct AxStorage(ax_api::fs::AxFileHandle);

impl Storage for AxStorage {
    fn read_at(&mut self, offset: u64, bytes: &mut [u8]) -> VirtioResult<usize> {
        ax_api::fs::ax_read_file_at(&self.0, offset, bytes).map_err(|error| {
            log::error!("virtio-blk read at {offset:#x} failed: {error}");
            VirtioError::BackendError
        })
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> VirtioResult<usize> {
        ax_api::fs::ax_write_file_at(&self.0, offset, bytes).map_err(|error| {
            log::error!("virtio-blk write at {offset:#x} failed: {error}");
            VirtioError::BackendError
        })
    }

    fn sync(&mut self) -> VirtioResult<()> {
        ax_api::fs::ax_flush_file(&self.0).map_err(|error| {
            log::error!("virtio-blk flush failed: {error}");
            VirtioError::BackendError
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    Read,
    Write,
    Flush,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RequestKey {
    kind: Kind,
    offset: u64,
    len: usize,
}

struct Operation {
    key: RequestKey,
    bytes: Vec<u8>,
}

#[derive(Default)]
enum State {
    #[default]
    Idle,
    Queued(Operation),
    Running {
        key: RequestKey,
        cancelled: bool,
    },
    Complete {
        operation: Operation,
        result: VirtioResult<()>,
    },
}

#[derive(Default)]
struct Shared {
    state: State,
    stopping: bool,
}

impl Shared {
    fn cancel(&mut self) {
        match &mut self.state {
            State::Running { cancelled, .. } => *cancelled = true,
            _ => self.state = State::Idle,
        }
    }

    fn take_work(&mut self) -> Option<Operation> {
        if let State::Queued(operation) = &self.state {
            let running = State::Running {
                key: operation.key,
                cancelled: false,
            };
            if let State::Queued(operation) = core::mem::replace(&mut self.state, running) {
                return Some(operation);
            }
        }
        None
    }

    fn finish(&mut self, operation: Operation, result: VirtioResult<()>) {
        self.state = match self.state {
            State::Running {
                cancelled: false, ..
            } => State::Complete { operation, result },
            _ => State::Idle,
        };
    }
}

impl FileBackend {
    pub(super) fn new(
        file: ax_api::fs::AxFileHandle,
        capacity_sectors: u64,
        vm_id: usize,
    ) -> axdevice::DeviceManagerResult<Self> {
        Self::spawn(AxStorage(file), capacity_sectors, pin_worker, move || {
            // notify_vm publishes device work and requests execution on vCPU0.
            if let Err(error) = crate::AxvmRuntime::notify_vm(vm_id) {
                log::warn!("failed to notify VM[{vm_id}] of file I/O completion: {error}");
            }
        })
        .map_err(|error| axdevice::DeviceManagerError::InvalidConfig {
            operation: "spawn virtio-blk file worker",
            detail: format!("{error}"),
        })
    }

    fn spawn<S: Storage>(
        mut storage: S,
        capacity_sectors: u64,
        start: fn(),
        notify: impl Fn() + Send + 'static,
    ) -> std::io::Result<Self> {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let worker_shared = shared.clone();
        let worker = std::thread::Builder::new()
            .name("virtio-blk-file".into())
            .stack_size(WORKER_STACK_BYTES)
            .spawn(move || {
                start();
                loop {
                    // Never hold the state lock across storage I/O or notification.
                    let operation = {
                        let mut shared = worker_shared.lock().expect("file state mutex poisoned");
                        let work = shared.take_work();
                        if work.is_none() && shared.stopping {
                            break;
                        }
                        work
                    };
                    if let Some(mut operation) = operation {
                        let result = execute(&mut storage, &mut operation);
                        worker_shared
                            .lock()
                            .expect("file state mutex poisoned")
                            .finish(operation, result);
                        notify();
                    } else {
                        // unpark retains a token when it races this call.
                        std::thread::park();
                    }
                }
                if let Err(error) = storage.sync() {
                    log::error!("virtio-blk shutdown sync failed: {error:?}");
                }
            })?;
        Ok(Self {
            shared,
            worker: Some(worker),
            capacity_sectors,
        })
    }

    fn key(&self, kind: Kind, sector: u64, len: usize) -> VirtioResult<RequestKey> {
        if len > max_request_bytes() {
            return Err(VirtioError::InvalidBufferSize);
        }
        let offset = sector.checked_mul(512).ok_or(VirtioError::InvalidAddress)?;
        let end = offset
            .checked_add(len as u64)
            .ok_or(VirtioError::InvalidAddress)?;
        let capacity = self
            .capacity_sectors
            .checked_mul(512)
            .ok_or(VirtioError::InvalidAddress)?;
        if end > capacity {
            return Err(VirtioError::InvalidAddress);
        }
        Ok(RequestKey { kind, offset, len })
    }

    fn poll(&self, key: RequestKey, write_bytes: Option<&[u8]>) -> VirtioResult<Operation> {
        let mut shared = self.shared.lock().expect("file state mutex poisoned");
        if shared.stopping {
            return Err(VirtioError::DeviceNotReady);
        }
        match &shared.state {
            State::Idle => {
                let mut bytes = Vec::new();
                bytes
                    .try_reserve_exact(key.len)
                    .map_err(|_| VirtioError::MemoryError)?;
                bytes.resize(key.len, 0);
                if let Some(source) = write_bytes {
                    bytes.copy_from_slice(source);
                }
                shared.state = State::Queued(Operation { key, bytes });
                drop(shared);
                self.wake_worker();
                Err(VirtioError::WouldBlock)
            }
            State::Running {
                cancelled: true, ..
            } => Err(VirtioError::WouldBlock),
            State::Queued(operation) | State::Complete { operation, .. }
                if operation.key != key =>
            {
                shared.cancel();
                Err(VirtioError::InvalidRequest)
            }
            State::Running { key: running, .. } if *running != key => {
                shared.cancel();
                Err(VirtioError::InvalidRequest)
            }
            State::Queued(_) | State::Running { .. } => Err(VirtioError::WouldBlock),
            State::Complete { .. } => {
                let State::Complete { operation, result } = core::mem::take(&mut shared.state)
                else {
                    unreachable!()
                };
                result?;
                Ok(operation)
            }
        }
    }

    fn wake_worker(&self) {
        if let Some(worker) = &self.worker {
            worker.thread().unpark();
        }
    }
}

fn pin_worker() {
    let cpu = crate::host::cpu::current_id();
    if ax_api::task::ax_set_current_affinity(ax_api::task::AxCpuMask::one_shot(cpu)).is_err() {
        log::warn!("failed to pin virtio-blk file worker to CPU{cpu}");
    }
}

fn execute(storage: &mut impl Storage, operation: &mut Operation) -> VirtioResult<()> {
    if operation.key.kind == Kind::Flush {
        return storage.sync();
    }
    let mut done = 0;
    while done < operation.bytes.len() {
        let end = (done + IO_CHUNK_BYTES).min(operation.bytes.len());
        let offset = operation.key.offset + done as u64;
        let count = match operation.key.kind {
            Kind::Read => storage.read_at(offset, &mut operation.bytes[done..end])?,
            Kind::Write => storage.write_at(offset, &operation.bytes[done..end])?,
            Kind::Flush => unreachable!(),
        };
        if count == 0 || count > end - done {
            return Err(VirtioError::BackendError);
        }
        done += count;
    }
    Ok(())
}

impl BlockBackend for FileBackend {
    fn pending_request_ready(&self) -> bool {
        let shared = self.shared.lock().expect("file state mutex poisoned");
        shared.stopping || matches!(shared.state, State::Idle | State::Complete { .. })
    }

    fn requires_deferred_processing(&self) -> bool {
        true
    }

    fn read(&self, sector: u64, buffer: &mut [u8]) -> VirtioResult<usize> {
        let key = self.key(Kind::Read, sector, buffer.len())?;
        let completed = self.poll(key, None)?;
        buffer.copy_from_slice(&completed.bytes);
        Ok(buffer.len())
    }

    fn write(&self, sector: u64, buffer: &[u8]) -> VirtioResult<usize> {
        let key = self.key(Kind::Write, sector, buffer.len())?;
        self.poll(key, Some(buffer))?;
        Ok(buffer.len())
    }

    fn flush(&self) -> VirtioResult<()> {
        self.poll(
            RequestKey {
                kind: Kind::Flush,
                offset: 0,
                len: 0,
            },
            None,
        )?;
        Ok(())
    }

    fn cancel_pending_request(&self) {
        self.shared
            .lock()
            .expect("file state mutex poisoned")
            .cancel();
    }
}

impl Drop for FileBackend {
    fn drop(&mut self) {
        self.shared
            .lock()
            .expect("file state mutex poisoned")
            .stopping = true;
        self.wake_worker();
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            log::error!("virtio-blk file worker failed during teardown");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, time::Duration};

    use super::*;

    fn manual_backend() -> FileBackend {
        FileBackend {
            shared: Arc::new(Mutex::new(Shared::default())),
            worker: None,
            capacity_sectors: (1 << 40) / 512,
        }
    }

    #[test]
    fn failed_retry_cannot_complete_a_later_write_with_old_data() {
        use axvirtio_blk::{BlockDeviceEvent, VirtioMmioBlockDevice};
        use axvirtio_common::{GuestMemory, NoGuestMemoryAccessor};
        use axvm_types::{AccessWidth, GuestPhysAddr};

        struct Memory {
            bytes: Vec<u8>,
            fail_data: bool,
            read_calls: usize,
        }
        impl GuestMemory for Memory {
            fn read(&mut self, addr: GuestPhysAddr, bytes: &mut [u8]) -> VirtioResult<()> {
                self.read_calls += 1;
                if self.fail_data && addr.as_usize() == 0x400 {
                    return Err(VirtioError::MemoryError);
                }
                let start = addr.as_usize();
                bytes.copy_from_slice(&self.bytes[start..start + bytes.len()]);
                Ok(())
            }
            fn write(&mut self, addr: GuestPhysAddr, bytes: &[u8]) -> VirtioResult<()> {
                let start = addr.as_usize();
                self.bytes[start..start + bytes.len()].copy_from_slice(bytes);
                Ok(())
            }
        }
        for complete_before_failure in [false, true] {
            let backend = manual_backend();
            let shared = backend.shared.clone();
            let model = VirtioMmioBlockDevice::new(
                GuestPhysAddr::from(0x1000),
                0x200,
                backend,
                VirtioBlockConfig {
                    capacity: 8,
                    ..Default::default()
                },
                NoGuestMemoryAccessor,
            )
            .unwrap();
            let mut memory = Memory {
                bytes: vec![0; 0x1000],
                fail_data: false,
                read_calls: 0,
            };
            for (index, (address, len, flags, next)) in [
                (0x300_u64, 16_u32, 1_u16, 1_u16),
                (0x400, 512, 1, 2),
                (0x800, 1, 2, 0),
            ]
            .into_iter()
            .enumerate()
            {
                let start = 0x100 + index * 16;
                memory.bytes[start..start + 8].copy_from_slice(&address.to_le_bytes());
                memory.bytes[start + 8..start + 12].copy_from_slice(&len.to_le_bytes());
                memory.bytes[start + 12..start + 14].copy_from_slice(&flags.to_le_bytes());
                memory.bytes[start + 14..start + 16].copy_from_slice(&next.to_le_bytes());
            }
            memory.bytes[0x300..0x304].copy_from_slice(&1_u32.to_le_bytes());
            memory.bytes[0x400..0x600].fill(0x11);
            memory.bytes[0x202..0x204].copy_from_slice(&1_u16.to_le_bytes());
            for (register, value) in [
                (0x30, 0),
                (0x38, 4),
                (0x80, 0x100),
                (0x90, 0x200),
                (0xa0, 0x240),
                (0x44, 1),
            ] {
                model
                    .mmio_write_with_memory(
                        GuestPhysAddr::from(0x1000 + register),
                        AccessWidth::Dword,
                        value,
                        &mut memory,
                    )
                    .unwrap();
            }
            model.set_status(4);
            assert_eq!(
                model.process_pending_queue(0, &mut memory),
                Ok(BlockDeviceEvent::QueuePending(0))
            );
            let mut old = Some(shared.lock().unwrap().take_work().unwrap());
            let reads_before_wait = memory.read_calls;
            for _ in 0..32 {
                assert_eq!(
                    model.process_pending_queue(0, &mut memory),
                    Ok(BlockDeviceEvent::QueuePending(0))
                );
            }
            assert_eq!(
                memory.read_calls, reads_before_wait,
                "waiting for storage must not reread descriptors or copy guest data"
            );
            if complete_before_failure {
                shared.lock().unwrap().finish(old.take().unwrap(), Ok(()));
            }
            memory.fail_data = true;
            if !complete_before_failure {
                assert_eq!(
                    model.process_pending_queue(0, &mut memory),
                    Ok(BlockDeviceEvent::QueuePending(0))
                );
                shared.lock().unwrap().finish(old.take().unwrap(), Ok(()));
            }
            model.process_pending_queue(0, &mut memory).unwrap();
            assert_eq!(memory.bytes[0x800], 1, "the failed retry must report IOERR");
            if let Some(old) = old {
                shared.lock().unwrap().finish(old, Ok(()));
            }
            // Reuse the returned descriptor with the same range but new contents.
            memory.fail_data = false;
            memory.bytes[0x400..0x600].fill(0x22);
            memory.bytes[0x800] = 0xff;
            memory.bytes[0x202..0x204].copy_from_slice(&2_u16.to_le_bytes());
            assert_eq!(
                model.process_pending_queue(0, &mut memory),
                Ok(BlockDeviceEvent::QueuePending(0)),
                "a later write must issue fresh I/O, not consume the abandoned result"
            );
            assert_eq!(memory.bytes[0x800], 0xff);
            let fresh = shared
                .lock()
                .unwrap()
                .take_work()
                .expect("new storage write");
            assert_eq!(fresh.bytes, vec![0x22; 512]);
            shared.lock().unwrap().finish(fresh, Ok(()));
            model.process_pending_queue(0, &mut memory).unwrap();
            assert_eq!(memory.bytes[0x800], 0);
        }
    }

    #[test]
    fn read_waits_for_storage_and_returns_only_completed_bytes() {
        let backend = manual_backend();
        let mut bytes = [0xcc; 512];
        assert_eq!(backend.read(100, &mut bytes), Err(VirtioError::WouldBlock));
        assert_eq!(bytes, [0xcc; 512]);
        let mut work = backend.shared.lock().unwrap().take_work().unwrap();
        assert_eq!(work.key.offset, 100 * 512);
        assert_eq!(work.bytes.len(), 512);
        assert_eq!(backend.read(100, &mut bytes), Err(VirtioError::WouldBlock));
        work.bytes.fill(0x42);
        backend.shared.lock().unwrap().finish(work, Ok(()));
        assert_eq!(backend.read(100, &mut bytes), Ok(512));
        assert_eq!(bytes, [0x42; 512]);
        assert!(matches!(backend.shared.lock().unwrap().state, State::Idle));
    }

    #[test]
    fn writes_snapshot_once_and_report_storage_errors() {
        let backend = manual_backend();
        let mut bytes = [0x31; 512];
        assert_eq!(backend.write(7, &bytes), Err(VirtioError::WouldBlock));
        bytes.fill(0x99);
        assert_eq!(backend.write(7, &bytes), Err(VirtioError::WouldBlock));
        let work = backend.shared.lock().unwrap().take_work().unwrap();
        assert_eq!(work.bytes, [0x31; 512]);
        backend
            .shared
            .lock()
            .unwrap()
            .finish(work, Err(VirtioError::BackendError));
        assert_eq!(backend.write(7, &bytes), Err(VirtioError::BackendError));
        assert!(matches!(backend.shared.lock().unwrap().state, State::Idle));
    }

    #[test]
    fn flush_waits_for_sync_and_propagates_failure() {
        let backend = manual_backend();
        assert_eq!(backend.flush(), Err(VirtioError::WouldBlock));
        let work = backend.shared.lock().unwrap().take_work().unwrap();
        assert_eq!(work.key.kind, Kind::Flush);
        assert!(work.bytes.is_empty());
        assert_eq!(backend.flush(), Err(VirtioError::WouldBlock));
        backend
            .shared
            .lock()
            .unwrap()
            .finish(work, Err(VirtioError::BackendError));
        assert_eq!(backend.flush(), Err(VirtioError::BackendError));
    }

    #[test]
    fn reset_drains_old_io_without_reusing_its_completion() {
        let backend = manual_backend();
        let mut bytes = [0; 512];
        assert_eq!(backend.read(0, &mut bytes), Err(VirtioError::WouldBlock));
        backend.reset();
        assert!(backend.shared.lock().unwrap().take_work().is_none());
        assert_eq!(backend.read(0, &mut bytes), Err(VirtioError::WouldBlock));
        let old = backend.shared.lock().unwrap().take_work().unwrap();
        backend.reset();
        assert_eq!(backend.read(0, &mut bytes), Err(VirtioError::WouldBlock));
        backend.shared.lock().unwrap().finish(old, Ok(()));
        // Even an identical request after reset must issue fresh storage I/O.
        assert_eq!(backend.read(0, &mut bytes), Err(VirtioError::WouldBlock));
        let fresh = backend.shared.lock().unwrap().take_work().unwrap();
        backend.shared.lock().unwrap().finish(fresh, Ok(()));
        backend.reset();
        assert_eq!(backend.read(0, &mut bytes), Err(VirtioError::WouldBlock));
    }

    #[test]
    fn oversized_and_out_of_range_io_are_rejected_before_queueing() {
        let backend = manual_backend();
        assert_eq!(
            backend.key(Kind::Read, 0, max_request_bytes() + 1),
            Err(VirtioError::InvalidBufferSize)
        );
        assert_eq!(
            backend.key(Kind::Read, u64::MAX, 512),
            Err(VirtioError::InvalidAddress)
        );
        assert_eq!(
            backend.read(backend.capacity_sectors, &mut [0; 512]),
            Err(VirtioError::InvalidAddress)
        );
        assert!(matches!(backend.shared.lock().unwrap().state, State::Idle));
    }

    struct PartialStorage {
        bytes: Vec<u8>,
        fail: bool,
        zero: bool,
    }
    impl Storage for PartialStorage {
        fn read_at(&mut self, offset: u64, bytes: &mut [u8]) -> VirtioResult<usize> {
            if self.fail {
                return Err(VirtioError::BackendError);
            }
            if self.zero {
                return Ok(0);
            }
            let count = bytes.len().min(17);
            bytes[..count].copy_from_slice(&self.bytes[offset as usize..offset as usize + count]);
            Ok(count)
        }
        fn write_at(&mut self, offset: u64, bytes: &[u8]) -> VirtioResult<usize> {
            if self.fail {
                return Err(VirtioError::BackendError);
            }
            if self.zero {
                return Ok(0);
            }
            let count = bytes.len().min(17);
            self.bytes[offset as usize..offset as usize + count].copy_from_slice(&bytes[..count]);
            Ok(count)
        }
        fn sync(&mut self) -> VirtioResult<()> {
            Ok(())
        }
    }

    #[test]
    fn partial_io_advances_offsets_and_zero_progress_is_an_error() {
        let mut storage = PartialStorage {
            bytes: vec![0; 8192],
            fail: false,
            zero: false,
        };
        let mut operation = Operation {
            key: RequestKey {
                kind: Kind::Write,
                offset: 512,
                len: 4608,
            },
            bytes: vec![0x5a; 4608],
        };
        execute(&mut storage, &mut operation).unwrap();
        assert_eq!(&storage.bytes[512..512 + 4608], &[0x5a; 4608]);
        operation.key.kind = Kind::Read;
        operation.bytes.fill(0);
        execute(&mut storage, &mut operation).unwrap();
        assert_eq!(operation.bytes, vec![0x5a; 4608]);
        storage.zero = true;
        assert_eq!(
            execute(&mut storage, &mut operation),
            Err(VirtioError::BackendError)
        );
        operation.key.kind = Kind::Write;
        assert_eq!(
            execute(&mut storage, &mut operation),
            Err(VirtioError::BackendError)
        );
        storage.fail = true;
        assert_eq!(
            execute(&mut storage, &mut operation),
            Err(VirtioError::BackendError)
        );
    }

    #[cfg(unix)]
    #[test]
    fn sparse_large_file_roundtrip_uses_real_worker_and_flush() {
        use std::os::unix::fs::FileExt;
        struct HostFile(std::fs::File);
        impl Storage for HostFile {
            fn read_at(&mut self, offset: u64, bytes: &mut [u8]) -> VirtioResult<usize> {
                self.0
                    .read_at(bytes, offset)
                    .map_err(|_| VirtioError::BackendError)
            }
            fn write_at(&mut self, offset: u64, bytes: &[u8]) -> VirtioResult<usize> {
                self.0
                    .write_at(bytes, offset)
                    .map_err(|_| VirtioError::BackendError)
            }
            fn sync(&mut self) -> VirtioResult<()> {
                self.0.sync_all().map_err(|_| VirtioError::BackendError)
            }
        }
        let path = std::env::temp_dir().join(format!("axvm-sparse-{}.img", std::process::id()));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        // Unlink immediately: the open descriptors own this disposable fixture.
        std::fs::remove_file(&path).unwrap();
        let capacity = 64_u64 << 30;
        file.set_len(capacity).unwrap();
        let observer = file.try_clone().unwrap();
        let (send, receive) = mpsc::channel();
        let backend = FileBackend::spawn(
            HostFile(file),
            capacity / 512,
            || {},
            move || {
                let _ = send.send(());
            },
        )
        .unwrap();
        let sector = capacity / 512 - 1;
        assert_eq!(
            backend.write(sector, &[0x4b; 512]),
            Err(VirtioError::WouldBlock)
        );
        receive.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(backend.write(sector, &[0x4b; 512]), Ok(512));
        assert_eq!(backend.flush(), Err(VirtioError::WouldBlock));
        receive.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(backend.flush(), Ok(()));
        let mut bytes = [0; 512];
        assert_eq!(observer.read_at(&mut bytes, capacity - 512).unwrap(), 512);
        assert_eq!(bytes, [0x4b; 512]);
        assert_eq!(
            backend.read(sector, &mut bytes),
            Err(VirtioError::WouldBlock)
        );
        receive.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(backend.read(sector, &mut bytes), Ok(512));
        assert_eq!(bytes, [0x4b; 512]);
        drop(backend);
    }
}
