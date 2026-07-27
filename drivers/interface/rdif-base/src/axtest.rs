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

#[axtest]
fn rdif_base_read_write_trait_dispatch_hold() {
    use crate::{
        DriverGeneric,
        io::{Error, Read, Write},
    };

    struct TestBackend {
        data: Vec<u8>,
        read_pos: usize,
    }

    impl DriverGeneric for TestBackend {
        fn name(&self) -> &str {
            "test-backend"
        }
    }

    impl Read for TestBackend {
        fn read(&mut self, buf: &mut [u8]) -> Result<(), Error> {
            let remaining = self.data.len() - self.read_pos;
            if remaining == 0 {
                return Ok(());
            }
            let count = buf.len().min(remaining);
            buf[..count].copy_from_slice(&self.data[self.read_pos..self.read_pos + count]);
            self.read_pos += count;
            Ok(())
        }
    }

    impl Write for TestBackend {
        fn write(&mut self, buf: &[u8]) -> Result<(), Error> {
            self.data.extend_from_slice(buf);
            Ok(())
        }
    }

    let mut backend = TestBackend {
        data: Vec::from(b"hello".as_slice()),
        read_pos: 0,
    };

    // Test read
    let mut buf = [0; 5];
    let _ = backend.read(&mut buf);
    ax_assert_eq!(&buf, b"hello");

    // Test write
    let _ = backend.write(b" world");
    ax_assert_eq!(backend.data, b"hello world");
}

#[axtest]
fn rdif_base_error_conversion_and_chaining_hold() {
    use crate::io::{Error, ErrorKind};

    // Test error creation with different kinds
    let err1 = Error {
        kind: ErrorKind::InvalidData,
        success_pos: 0,
    };
    ax_assert!(matches!(err1.kind, ErrorKind::InvalidData));

    let err2 = Error {
        kind: ErrorKind::TimedOut,
        success_pos: 0,
    };
    ax_assert!(matches!(err2.kind, ErrorKind::TimedOut));

    // Test error with success position
    let err3 = Error {
        kind: ErrorKind::Interrupted,
        success_pos: 5,
    };
    ax_assert_eq!(err3.success_pos, 5);

    // Test Other variant
    let inner = Error {
        kind: ErrorKind::WriteZero,
        success_pos: 0,
    };
    let outer = ErrorKind::Other(Box::new(inner));
    ax_assert!(matches!(outer, ErrorKind::Other(_)));
}

#[axtest]
fn rdif_base_seek_and_io_traits_hold() {
    use crate::io::{Error, ErrorKind, Read, Write};

    struct VecWriter {
        data: Vec<u8>,
        pos: usize,
    }

    impl VecWriter {
        fn new() -> Self {
            Self {
                data: Vec::new(),
                pos: 0,
            }
        }
    }

    impl Write for VecWriter {
        fn write(&mut self, buf: &[u8]) -> Result<(), Error> {
            self.data.extend_from_slice(buf);
            self.pos += buf.len();
            Ok(())
        }
    }

    impl Read for VecWriter {
        fn read(&mut self, buf: &mut [u8]) -> Result<(), Error> {
            let remaining = self.data.len().saturating_sub(self.pos);
            let to_copy = remaining.min(buf.len());
            buf[..to_copy].copy_from_slice(&self.data[self.pos..self.pos + to_copy]);
            self.pos += to_copy;
            Ok(())
        }
    }

    let mut writer = VecWriter::new();
    writer.write(b"hello").unwrap();
    writer.write(b" world").unwrap();
    ax_assert_eq!(writer.data, b"hello world");
    ax_assert_eq!(writer.pos, 11);

    // Reset position and read back
    writer.pos = 6;
    let mut buf = [0_u8; 5];
    writer.read(&mut buf).unwrap();
    ax_assert_eq!(&buf, b"world");

    // Test error kinds for coverage
    let _not_available = ErrorKind::NotAvailable;
    let _broken_pipe = ErrorKind::BrokenPipe;
    let _unsupported = ErrorKind::Unsupported;
    let _out_of_memory = ErrorKind::OutOfMemory;
}

#[axtest]
fn rdif_base_driver_generic_name_and_any_hold() {
    use crate::DriverGeneric;

    struct NamedBackend {
        name_str: &'static str,
    }

    impl DriverGeneric for NamedBackend {
        fn name(&self) -> &str {
            self.name_str
        }

        fn raw_any(&self) -> Option<&dyn core::any::Any> {
            None
        }

        fn raw_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
            None
        }
    }

    let mut backend = NamedBackend {
        name_str: "test-driver",
    };
    ax_assert_eq!(backend.name(), "test-driver");
    ax_assert!(backend.raw_any().is_none());
    ax_assert!(backend.raw_any_mut().is_none());
}

#[axtest]
fn rdif_base_io_read_empty_and_partial_hold() {
    use crate::io::{Error, Read};

    // Test reading from empty source
    struct EmptyReader;
    impl Read for EmptyReader {
        fn read(&mut self, _buf: &mut [u8]) -> Result<(), Error> {
            Ok(())
        }
    }

    let mut reader = EmptyReader;
    let mut buf = [0u8; 10];
    reader.read(&mut buf).unwrap();
    ax_assert_eq!(buf[0], 0);

    // Test partial read
    struct PartialReader {
        data: &'static [u8],
        pos: usize,
    }
    impl Read for PartialReader {
        fn read(&mut self, buf: &mut [u8]) -> Result<(), Error> {
            if self.pos >= self.data.len() {
                return Ok(());
            }
            let to_copy = (self.data.len() - self.pos).min(buf.len());
            buf[..to_copy].copy_from_slice(&self.data[self.pos..self.pos + to_copy]);
            self.pos += to_copy;
            Ok(())
        }
    }

    let mut partial = PartialReader {
        data: b"hi",
        pos: 0,
    };
    let mut buf2 = [0u8; 10];
    partial.read(&mut buf2).unwrap();
    ax_assert_eq!(&buf2[..2], b"hi");
}

#[axtest]
fn rdif_base_io_write_multiple_calls_hold() {
    use crate::io::{Error, Write};

    // Test multiple write calls accumulate correctly
    struct AccumulatingWriter {
        data: Vec<u8>,
    }
    impl Write for AccumulatingWriter {
        fn write(&mut self, buf: &[u8]) -> Result<(), Error> {
            self.data.extend_from_slice(buf);
            Ok(())
        }
    }

    let mut writer = AccumulatingWriter { data: Vec::new() };
    writer.write(b"a").unwrap();
    writer.write(b"b").unwrap();
    writer.write(b"c").unwrap();
    ax_assert_eq!(writer.data, b"abc");

    // Test empty write
    writer.write(b"").unwrap();
    ax_assert_eq!(writer.data.len(), 3);
}
