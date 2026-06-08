#![allow(async_fn_in_trait)]

use alloc::boxed::Box;
use core::{fmt::Display, future, task::Poll};

pub trait Read {
    /// Read data from the device.
    fn read(&mut self, buf: &mut [u8]) -> Result;

    /// Read data from the device, blocking until all bytes are read
    fn read_all_blocking(&mut self, buf: &mut [u8]) -> Result {
        let mut n = 0;
        while n < buf.len() {
            let tmp = &mut buf[n..];
            if let Err(mut e) = self.read(tmp) {
                n += e.success_pos;
                if matches!(e.kind, ErrorKind::Interrupted) {
                    continue;
                } else {
                    e.success_pos = n;
                    return Err(e);
                }
            } else {
                n += tmp.len();
            }
        }

        Ok(())
    }

    async fn read_all(&mut self, buf: &mut [u8]) -> Result {
        let mut n = 0;
        future::poll_fn(move |cx| {
            let tmp = &mut buf[n..];
            if let Err(mut e) = self.read(tmp) {
                n += e.success_pos;
                if !matches!(e.kind, ErrorKind::Interrupted) {
                    e.success_pos = n;
                    return Poll::Ready(Err(e));
                }
            } else {
                n += tmp.len();
            }
            if n == buf.len() {
                Poll::Ready(Ok(()))
            } else {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        })
        .await
    }
}

pub trait Write {
    /// Write data to the device.
    fn write(&mut self, buf: &[u8]) -> Result;

    fn write_all_blocking(&mut self, buf: &[u8]) -> Result {
        let mut n = 0;
        while n < buf.len() {
            let tmp = &buf[n..];
            if let Err(mut e) = self.write(tmp) {
                n += e.success_pos;
                if matches!(e.kind, ErrorKind::Interrupted) {
                    continue;
                } else {
                    e.success_pos = n;
                    return Err(e);
                }
            } else {
                n += tmp.len();
            }
        }
        Ok(())
    }

    async fn write_all(&mut self, buf: &[u8]) -> Result {
        let mut n = 0;
        future::poll_fn(move |cx| {
            let tmp = &buf[n..];
            if let Err(mut e) = self.write(tmp) {
                n += e.success_pos;
                if !matches!(e.kind, ErrorKind::Interrupted) {
                    e.success_pos = n;
                    return Poll::Ready(Err(e));
                }
            } else {
                n += tmp.len();
            }
            if n == buf.len() {
                Poll::Ready(Ok(()))
            } else {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        })
        .await
    }
}

pub type Result<T = ()> = core::result::Result<T, Error>;

/// Io error
#[derive(Debug)]
pub struct Error {
    /// The kind of error
    pub kind: ErrorKind,
    /// The position of the valid data
    pub success_pos: usize,
}

impl Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "success pos {}, err:{}", self.success_pos, self.kind)
    }
}

impl core::error::Error for Error {}

/// Io error kind
#[derive(Debug)]
pub enum ErrorKind {
    Other(Box<dyn core::error::Error>),
    NotAvailable,
    BrokenPipe,
    InvalidParameter {
        name: &'static str,
    },
    InvalidData,
    TimedOut,
    /// This operation was interrupted.
    ///
    /// Interrupted operations can typically be retried.
    Interrupted,
    /// This operation is unsupported on this platform.
    ///
    /// This means that the operation can never succeed.
    Unsupported,
    /// An operation could not be completed, because it failed
    /// to allocate enough memory.
    OutOfMemory,
    /// An attempted write could not write any data.
    WriteZero,
}

impl Display for ErrorKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Other(e) => write!(f, "Other error: {e}"),
            Self::NotAvailable => write!(f, "Hardware not available"),
            Self::BrokenPipe => write!(f, "Broken pipe"),
            Self::InvalidParameter { name } => write!(f, "Invalid parameter: {name}"),
            Self::InvalidData => write!(f, "Invalid data"),
            Self::TimedOut => write!(f, "Timed out"),
            Self::Interrupted => write!(f, "Interrupted"),
            Self::Unsupported => write!(f, "Unsupported"),
            Self::OutOfMemory => write!(f, "Out of memory"),
            Self::WriteZero => write!(f, "Write zero"),
        }
    }
}

impl core::error::Error for ErrorKind {}

#[cfg(test)]
mod test {

    use super::*;

    struct TRead;

    impl Read for TRead {
        fn read(&mut self, buf: &mut [u8]) -> Result {
            const MAX: usize = 2;
            if !buf.is_empty() {
                buf[0] = 1;
            }
            if buf.len() > 1 {
                buf[1] = 1;
            }
            if buf.len() > MAX {
                return Err(Error {
                    kind: ErrorKind::Interrupted,
                    success_pos: MAX,
                });
            }
            Ok(())
        }
    }

    struct ARead<'a, 'b> {
        n: usize,
        buf: &'a mut [u8],
        p: &'b mut TRead2,
    }

    impl Future for ARead<'_, '_> {
        type Output = Result;

        fn poll(
            mut self: core::pin::Pin<&mut Self>,
            cx: &mut core::task::Context<'_>,
        ) -> Poll<Self::Output> {
            let this = &mut *self;
            let ARead { n, buf, p } = this;

            let tmp = &mut buf[*n..];
            if let Err(mut e) = p.read(tmp) {
                *n += e.success_pos;
                if !matches!(e.kind, ErrorKind::Interrupted) {
                    e.success_pos = *n;
                    return Poll::Ready(Err(e));
                }
            } else {
                *n += tmp.len();
            }
            if *n == buf.len() {
                Poll::Ready(Ok(()))
            } else {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }

    struct TRead2;

    impl Read for TRead2 {
        #[doc = " Read data from the device."]
        fn read(&mut self, buf: &mut [u8]) -> Result {
            const MAX: usize = 2;
            if !buf.is_empty() {
                buf[0] = 1;
            }
            if buf.len() > 1 {
                buf[1] = 1;
            }
            if buf.len() > MAX {
                return Err(Error {
                    kind: ErrorKind::Interrupted,
                    success_pos: MAX,
                });
            }
            Ok(())
        }

        fn read_all<'life0, 'life1, 'async_trait>(
            &'life0 mut self,
            buf: &'life1 mut [u8],
        ) -> ::core::pin::Pin<Box<dyn ::core::future::Future<Output = Result> + 'async_trait + Send>>
        where
            'life0: 'async_trait,
            'life1: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(ARead { n: 0, buf, p: self })
        }
    }

    struct TWrite {
        data: [u8; 8],
        iter: usize,
    }

    impl TWrite {
        fn new() -> Self {
            Self {
                data: [0; 8],
                iter: 0,
            }
        }

        fn put(&mut self, data: u8) -> core::result::Result<(), ErrorKind> {
            if self.iter >= self.data.len() {
                return Err(ErrorKind::BrokenPipe);
            }
            self.data[self.iter] = data;
            self.iter += 1;
            Ok(())
        }
    }

    impl Write for TWrite {
        fn write(&mut self, buf: &[u8]) -> Result {
            const MAX: usize = 2;
            for (n, i) in (0..MAX.min(buf.len())).enumerate() {
                self.put(buf[i]).map_err(|e| Error {
                    kind: e,
                    success_pos: n,
                })?;
            }
            if buf.len() > MAX {
                return Err(Error {
                    kind: ErrorKind::Interrupted,
                    success_pos: MAX,
                });
            }

            Ok(())
        }
    }

    #[test]
    fn test_r() {
        let mut buf = [0; 8];
        let mut read = TRead;
        read.read_all_blocking(&mut buf).unwrap();

        assert_eq!(buf, [1; 8]);
    }

    #[tokio::test]
    async fn test_async_r() {
        let mut buf = [0; 8];

        let buf = tokio::spawn(async move {
            let mut read = TRead;
            read.read_all(&mut buf).await.unwrap();
            buf
        })
        .await
        .unwrap();

        assert_eq!(buf, [1; 8]);
    }

    #[tokio::test]
    async fn test_async_r2() {
        let mut buf = [0; 8];

        let mut read = TRead2;
        read.read_all(&mut buf).await.unwrap();

        assert_eq!(buf, [1; 8]);
    }

    #[test]
    fn test_w() {
        let buf = [1; 8];
        let mut w = TWrite::new();
        w.write_all_blocking(&buf).unwrap();

        assert_eq!(buf, w.data);
    }

    #[tokio::test]
    async fn test_async_w() {
        let buf = [1; 8];
        let mut w = TWrite::new();
        w.write_all(&buf).await.unwrap();

        assert_eq!(buf, w.data);
    }
}
