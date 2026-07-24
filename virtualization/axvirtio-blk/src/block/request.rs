use crate::backend::BlockBackend;
use crate::constants::*;
use alloc::{sync::Arc, vec::Vec};
use axaddrspace::{GuestMemoryAccessor, GuestPhysAddr};
use axvirtio_common::{VirtioError, VirtioResult};

/// Block request types
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockRequestType {
    /// Read request
    Read,
    /// Write request
    Write,
    /// Flush request
    Flush,
    /// Unsupported request
    #[default]
    Unsupported,
}

impl From<u32> for BlockRequestType {
    fn from(value: u32) -> Self {
        match value {
            VIRTIO_BLK_T_IN => BlockRequestType::Read,
            VIRTIO_BLK_T_OUT => BlockRequestType::Write,
            VIRTIO_BLK_T_FLUSH => BlockRequestType::Flush,
            _ => BlockRequestType::Unsupported, // Default to Unsupported for unknown types
        }
    }
}

impl From<BlockRequestType> for u32 {
    fn from(request_type: BlockRequestType) -> Self {
        match request_type {
            BlockRequestType::Read => VIRTIO_BLK_T_IN,
            BlockRequestType::Write => VIRTIO_BLK_T_OUT,
            BlockRequestType::Flush => VIRTIO_BLK_T_FLUSH,
            BlockRequestType::Unsupported => {
                panic!("Unsupported request type cannot be converted to u32")
            }
        }
    }
}

/// Data source for block requests
#[derive(Debug)]
pub enum DataSource {
    /// Guest memory buffers (for VirtIO protocol)
    GuestMemory {
        buffers: Vec<(GuestPhysAddr, usize, bool)>, // (addr, len, is_write)
        status_addr: GuestPhysAddr,
    },
}

/// Block request processing result
#[derive(Debug, Clone, Copy)]
pub enum BlockRequestResult {
    /// Request completed successfully
    Ok = VIRTIO_BLK_S_OK,
    /// I/O error occurred
    IoError = VIRTIO_BLK_S_IOERR,
    /// Unsupported request type
    Unsupported = VIRTIO_BLK_S_UNSUPP,
}

/// Unified block request structure
#[derive(Debug)]
pub struct BlockRequest<T: GuestMemoryAccessor + Clone> {
    /// Request type
    pub request_type: u32,
    /// Starting sector
    pub sector: u64,
    /// Data source (buffer or guest memory)
    pub data_source: DataSource,
    /// Guest memory accessor
    accessor: Arc<T>,
}

impl<T: GuestMemoryAccessor + Clone> BlockRequest<T> {
    /// Create a new block request with guest memory buffers
    pub fn new_virtio(
        request_type: u32,
        sector: u64,
        buffers: Vec<(GuestPhysAddr, usize, bool)>,
        status_addr: GuestPhysAddr,
        accessor: Arc<T>,
    ) -> Self {
        Self {
            request_type,
            sector,
            data_source: DataSource::GuestMemory {
                buffers,
                status_addr,
            },
            accessor,
        }
    }

    /// Get the size of the request in bytes
    pub fn size(&self) -> usize {
        match &self.data_source {
            DataSource::GuestMemory { buffers, .. } => buffers.iter().map(|(_, len, _)| *len).sum(),
        }
    }

    /// Execute the request and return result
    pub fn execute(&self, backend: &dyn BlockBackend) -> VirtioResult<BlockRequestResult> {
        match &self.data_source {
            DataSource::GuestMemory {
                buffers,
                status_addr,
            } => {
                let status = self.execute_guest_memory_request(backend, buffers, *status_addr)?;
                // Status byte writing is handled by the device layer when completing the request
                Ok(status)
            }
        }
    }

    /// Execute request with guest memory buffers
    fn execute_guest_memory_request(
        &self,
        backend: &dyn BlockBackend,
        buffers: &[(GuestPhysAddr, usize, bool)],
        _status_addr: GuestPhysAddr,
    ) -> VirtioResult<BlockRequestResult> {
        match BlockRequestType::from(self.request_type) {
            BlockRequestType::Read => self.handle_read_request_guest_memory(backend, buffers),
            BlockRequestType::Write => self.handle_write_request_guest_memory(backend, buffers),
            BlockRequestType::Flush => self.handle_flush_request_guest_memory(backend),
            BlockRequestType::Unsupported => Ok(BlockRequestResult::Unsupported),
        }
    }

    /// Handle read request with guest memory
    fn handle_read_request_guest_memory(
        &self,
        backend: &dyn BlockBackend,
        buffers: &[(GuestPhysAddr, usize, bool)],
    ) -> VirtioResult<BlockRequestResult> {
        let total_len = self.size();
        let mut buffer = vec![0u8; total_len];

        // Read data from backend
        match backend.read(self.sector, &mut buffer) {
            Ok(bytes_read) => {
                trace!(
                    "Read {bytes_read} bytes from backend at sector {0}",
                    self.sector
                );

                // Copy data to guest memory buffers
                let mut buffer_offset = 0;
                for (guest_addr, len, is_write) in buffers {
                    if !is_write {
                        warn!("Read request has non-writable data buffer");
                        continue;
                    }

                    let end_offset = buffer_offset + len;
                    if end_offset > buffer.len() {
                        error!("Data buffer exceeds read data range");
                        return Err(VirtioError::InvalidBufferSize);
                    }

                    // Write data to guest memory using injected memory accessor
                    if let Err(e) = self
                        .accessor
                        .write_buffer(*guest_addr, &buffer[buffer_offset..end_offset])
                    {
                        error!("Failed to write data to guest memory: {:?}", e);
                        return Err(VirtioError::MemoryError);
                    }

                    buffer_offset = end_offset;
                }

                Ok(BlockRequestResult::Ok)
            }
            Err(e) => {
                error!("Failed to read from backend: {:?}", e);
                Err(VirtioError::BackendError)
            }
        }
    }

    /// Handle write request with guest memory
    fn handle_write_request_guest_memory(
        &self,
        backend: &dyn BlockBackend,
        buffers: &[(GuestPhysAddr, usize, bool)],
    ) -> VirtioResult<BlockRequestResult> {
        let total_len = self.size();
        let mut buffer = vec![0u8; total_len];
        let mut buffer_offset = 0;

        // Read data from guest memory buffers
        for (guest_addr, len, is_write) in buffers {
            if *is_write {
                warn!("Write request has writable data buffer");
                continue;
            }

            let end_offset = buffer_offset + len;
            if end_offset > buffer.len() {
                error!("Data buffer exceeds write data range");
                return Err(VirtioError::InvalidBufferSize);
            }

            // Read data from guest memory using injected memory accessor
            if let Err(e) = self
                .accessor
                .read_buffer(*guest_addr, &mut buffer[buffer_offset..end_offset])
            {
                error!("Failed to read data from guest memory: {:?}", e);
                return Err(VirtioError::MemoryError);
            }

            buffer_offset = end_offset;
        }

        // Write data to backend
        match backend.write(self.sector, &buffer) {
            Ok(bytes_written) => {
                trace!(
                    "Wrote {} bytes to backend at sector {}",
                    bytes_written, self.sector
                );
                Ok(BlockRequestResult::Ok)
            }
            Err(e) => {
                error!("Failed to write to backend: {:?}", e);
                Err(VirtioError::BackendError)
            }
        }
    }

    /// Handle flush request with guest memory
    fn handle_flush_request_guest_memory(
        &self,
        backend: &dyn BlockBackend,
    ) -> VirtioResult<BlockRequestResult> {
        // Flush the backend
        match backend.flush() {
            Ok(_) => {
                debug!("Flushed backend");
                Ok(BlockRequestResult::Ok)
            }
            Err(e) => {
                error!("Failed to flush backend: {:?}", e);
                Err(VirtioError::BackendError)
            }
        }
    }
}
