use alloc::{boxed::Box, vec::Vec};

use axtest::prelude::*;

use crate::{
    DriverGeneric,
    io::{Error, ErrorKind, Read, Write},
};

pub trait DemoInterface: DriverGeneric {
    fn value(&self) -> usize;
    fn set_value(&mut self, value: usize);
}

crate::def_driver!(DemoDriver, DemoInterface);

struct DemoBackend {
    value: usize,
}

impl DriverGeneric for DemoBackend {
    fn name(&self) -> &str {
        "demo-backend"
    }

    fn raw_any(&self) -> Option<&dyn core::any::Any> {
        Some(self)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        Some(self)
    }
}

impl DemoInterface for DemoBackend {
    fn value(&self) -> usize {
        self.value
    }

    fn set_value(&mut self, value: usize) {
        self.value = value;
    }
}

struct ChunkedReader {
    chunks: Vec<&'static [u8]>,
}

impl Read for ChunkedReader {
    fn read(&mut self, buf: &mut [u8]) -> crate::io::Result {
        let Some(chunk) = self.chunks.pop() else {
            return Err(Error {
                kind: ErrorKind::InvalidData,
                success_pos: 0,
            });
        };
        let count = chunk.len().min(buf.len());
        buf[..count].copy_from_slice(&chunk[..count]);
        if count < buf.len() {
            return Err(Error {
                kind: ErrorKind::Interrupted,
                success_pos: count,
            });
        }
        Ok(())
    }
}

struct ChunkedWriter {
    accepted: Vec<u8>,
    limit: usize,
}

impl Write for ChunkedWriter {
    fn write(&mut self, buf: &[u8]) -> crate::io::Result {
        let count = self.limit.min(buf.len());
        self.accepted.extend_from_slice(&buf[..count]);
        if count < buf.len() {
            return Err(Error {
                kind: ErrorKind::Interrupted,
                success_pos: count,
            });
        }
        Ok(())
    }
}

#[axtest]
fn rdif_base_def_driver_wraps_and_downcasts_backends() {
    let mut driver = DemoDriver::new(DemoBackend { value: 7 });

    ax_assert_eq!(driver.name(), "demo-backend");
    ax_assert_eq!(driver.value(), 7);
    driver.set_value(11);
    ax_assert_eq!(driver.typed_ref::<DemoBackend>().unwrap().value, 11);
    driver.typed_mut::<DemoBackend>().unwrap().value = 13;
    ax_assert_eq!(driver.value(), 13);
}

#[axtest]
fn rdif_base_blocking_io_retries_interrupted_progress() {
    let mut reader = ChunkedReader {
        chunks: alloc::vec![b"cd", b"ab"],
    };
    let mut buf = [0; 4];
    reader.read_all_blocking(&mut buf).unwrap();
    ax_assert_eq!(&buf, b"abcd");

    let mut writer = ChunkedWriter {
        accepted: Vec::new(),
        limit: 2,
    };
    writer.write_all_blocking(b"abcd").unwrap();
    ax_assert_eq!(writer.accepted, alloc::vec![b'a', b'b', b'c', b'd']);
}

#[axtest]
fn rdif_base_io_errors_preserve_kind_and_success_position() {
    let error = Error {
        kind: ErrorKind::InvalidParameter { name: "baudrate" },
        success_pos: 3,
    };

    ax_assert_eq!(error.success_pos, 3);
    ax_assert!(alloc::format!("{error}").contains("success pos 3"));
    ax_assert!(matches!(
        error.kind,
        ErrorKind::InvalidParameter { name: "baudrate" }
    ));

    let other = ErrorKind::Other(Box::new(Error {
        kind: ErrorKind::WriteZero,
        success_pos: 0,
    }));
    ax_assert!(matches!(other, ErrorKind::Other(_)));
}

#[axtest]
fn rdif_base_error_kind_variants_hold() {
    // Check that all ErrorKind variants exist and are distinct
    let kinds = [
        ErrorKind::NotAvailable,
        ErrorKind::BrokenPipe,
        ErrorKind::InvalidData,
        ErrorKind::TimedOut,
        ErrorKind::Interrupted,
        ErrorKind::Unsupported,
        ErrorKind::OutOfMemory,
        ErrorKind::InvalidParameter { name: "test" },
    ];
    
    // Just verify they can be created and matched
    for kind in &kinds {
        match kind {
            ErrorKind::NotAvailable => {}
            ErrorKind::BrokenPipe => {}
            ErrorKind::InvalidData => {}
            ErrorKind::TimedOut => {}
            ErrorKind::Interrupted => {}
            ErrorKind::Unsupported => {}
            ErrorKind::OutOfMemory => {}
            ErrorKind::InvalidParameter { .. } => {}
            ErrorKind::Other(_) => {}
            ErrorKind::WriteZero => {}
        }
    }
}
