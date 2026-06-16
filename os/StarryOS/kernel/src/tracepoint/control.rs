use ax_sync::Mutex;
use axfs_ng_vfs::{VfsError, VfsResult};
use ktracepoint::{TraceFilterFile, TracePoint, TracePointEnableFile};

use crate::{
    pseudofs::DirectRwFsFileOps,
    tracepoint::{KernelExtTracePoint, KernelTraceAux},
};

/// File representing the `enable` attribute of a tracepoint event.
pub struct EventEnableObj {
    file: TracePointEnableFile,
    ext_tp: KernelExtTracePoint,
    tp: &'static TracePoint<KernelTraceAux>,
}

impl EventEnableObj {
    /// Create a new `EventEnableObj` instance.
    pub fn new(ext_tp: KernelExtTracePoint) -> Self {
        let tp = ext_tp.lock().trace_point();
        EventEnableObj {
            file: TracePointEnableFile::new(),
            ext_tp,
            tp,
        }
    }
}

impl DirectRwFsFileOps for EventEnableObj {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let enable_value = self.file.read(self.tp);
        let offset = offset as usize;
        if offset >= enable_value.len() {
            return Ok(0);
        }
        let len = buf.len().min(enable_value.len() - offset);
        buf[..len].copy_from_slice(&enable_value.as_bytes()[offset..offset + len]);
        Ok(len)
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        if buf.is_empty() {
            return Err(VfsError::InvalidInput);
        }
        let value = match core::str::from_utf8(buf)
            .map_err(|_| VfsError::InvalidInput)?
            .trim()
        {
            "0" => '0',
            "1" => '1',
            _ => return Err(VfsError::InvalidInput),
        };

        let mut ext_tp = self.ext_tp.lock();
        self.file.write(&mut ext_tp, value);
        Ok(buf.len())
    }
}

/// File representing the `filter` attribute of a tracepoint event.
pub struct EventFilterObj {
    file: Mutex<TraceFilterFile>,
    ext_tp: KernelExtTracePoint,
}

impl EventFilterObj {
    /// Create a new `EventFilterObj` instance.
    pub fn new(ext_tp: KernelExtTracePoint) -> Self {
        EventFilterObj {
            file: Mutex::new(TraceFilterFile::new()),
            ext_tp,
        }
    }
}

impl DirectRwFsFileOps for EventFilterObj {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let filter_value = self.file.lock().read();
        let offset = offset as usize;
        if offset >= filter_value.len() {
            return Ok(0);
        }
        let len = buf.len().min(filter_value.len() - offset);
        buf[..len].copy_from_slice(&filter_value.as_bytes()[offset..offset + len]);
        Ok(len)
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        let filter_str = core::str::from_utf8(buf).map_err(|_| VfsError::InvalidInput)?;
        let mut ext_tp = self.ext_tp.lock();
        self.file
            .lock()
            .write(&mut ext_tp, filter_str)
            .map_err(|_| VfsError::InvalidInput)?;
        Ok(buf.len())
    }
}

/// File representing the `max_record` attribute of the trace command line cache.
pub struct TraceCmdLineSizeObj;

impl DirectRwFsFileOps for TraceCmdLineSizeObj {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let max_record = super::TRACE_STATE.cmdline_cache.lock().max_record();
        let str = alloc::format!("{max_record}\n");
        let str_bytes = str.as_bytes();
        let offset = offset as usize;
        if offset >= str_bytes.len() {
            return Ok(0);
        }
        let len = buf.len().min(str_bytes.len() - offset);
        buf[..len].copy_from_slice(&str_bytes[offset..offset + len]);
        Ok(len)
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        let max_record_str = core::str::from_utf8(buf).map_err(|_| VfsError::InvalidInput)?;
        let max_record = max_record_str
            .trim_ascii()
            .parse()
            .map_err(|_| VfsError::InvalidInput)?;
        super::TRACE_STATE
            .cmdline_cache
            .lock()
            .set_max_record(max_record);
        Ok(buf.len())
    }
}
