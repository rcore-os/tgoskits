extern crate alloc;

use alloc::vec::Vec;

use rdif_base::{
    DriverGeneric,
    io::{Error, ErrorKind, Read, Write},
};

pub trait DemoInterface: DriverGeneric {
    fn value(&self) -> usize;
    fn set_value(&mut self, value: usize);
}

rdif_base::def_driver!(DemoDriver, DemoInterface);

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
    fn read(&mut self, buf: &mut [u8]) -> rdif_base::io::Result {
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
    fn write(&mut self, buf: &[u8]) -> rdif_base::io::Result {
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

#[test]
fn rdif_base_def_driver_wraps_and_downcasts_backends() {
    let mut driver = DemoDriver::new(DemoBackend { value: 7 });

    assert_eq!(driver.name(), "demo-backend");
    assert_eq!(driver.value(), 7);
    driver.set_value(11);
    assert_eq!(driver.typed_ref::<DemoBackend>().unwrap().value, 11);
    driver.typed_mut::<DemoBackend>().unwrap().value = 13;
    assert_eq!(driver.value(), 13);
}

#[test]
fn rdif_base_blocking_io_retries_interrupted_progress() {
    let mut reader = ChunkedReader {
        chunks: alloc::vec![b"cd", b"ab"],
    };
    let mut buf = [0; 4];
    reader.read_all_blocking(&mut buf).unwrap();
    assert_eq!(&buf, b"abcd");

    let mut writer = ChunkedWriter {
        accepted: Vec::new(),
        limit: 2,
    };
    writer.write_all_blocking(b"abcd").unwrap();
    assert_eq!(writer.accepted, alloc::vec![b'a', b'b', b'c', b'd']);
}
