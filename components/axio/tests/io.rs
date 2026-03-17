#![feature(core_io_borrowed_buf)]
#![feature(test)]

extern crate test;

use std::{
    io::{BorrowedBuf, BorrowedCursor},
    mem::MaybeUninit,
};

use axio::{BufRead, BufReader, Cursor, DEFAULT_BUF_SIZE, Error, Read, Result, Seek, SeekFrom};

#[cfg(feature = "alloc")]
#[test]
fn read_until() {
    let mut buf = Cursor::new(&b"12"[..]);
    let mut v = Vec::new();
    assert_eq!(buf.read_until(b'3', &mut v).unwrap(), 2);
    assert_eq!(v, b"12");

    let mut buf = Cursor::new(&b"1233"[..]);
    let mut v = Vec::new();
    assert_eq!(buf.read_until(b'3', &mut v).unwrap(), 3);
    assert_eq!(v, b"123");
    v.truncate(0);
    assert_eq!(buf.read_until(b'3', &mut v).unwrap(), 1);
    assert_eq!(v, b"3");
    v.truncate(0);
    assert_eq!(buf.read_until(b'3', &mut v).unwrap(), 0);
    assert_eq!(v, []);
}

#[cfg(feature = "alloc")]
#[test]
fn skip_until() {
    let bytes: &[u8] = b"read\0ignore\0read\0ignore\0read\0ignore\0";
    let mut reader = BufReader::new(bytes);

    // read from the bytes, alternating between
    // consuming `read\0`s and skipping `ignore\0`s
    loop {
        // consume `read\0`
        let mut out = Vec::new();
        let read = reader.read_until(0, &mut out).unwrap();
        if read == 0 {
            // eof
            break;
        } else {
            assert_eq!(out, b"read\0");
            assert_eq!(read, b"read\0".len());
        }

        // skip past `ignore\0`
        let skipped = reader.skip_until(0).unwrap();
        assert_eq!(skipped, b"ignore\0".len());
    }

    // ensure we are at the end of the byte slice and that we can skip no further
    // also ensure skip_until matches the behavior of read_until at EOF
    let skipped = reader.skip_until(0).unwrap();
    assert_eq!(skipped, 0);
}

#[cfg(feature = "alloc")]
#[test]
fn split() {
    let buf = Cursor::new(&b"12"[..]);
    let mut s = buf.split(b'3');
    assert_eq!(s.next().unwrap().unwrap(), vec![b'1', b'2']);
    assert!(s.next().is_none());

    let buf = Cursor::new(&b"1233"[..]);
    let mut s = buf.split(b'3');
    assert_eq!(s.next().unwrap().unwrap(), vec![b'1', b'2']);
    assert_eq!(s.next().unwrap().unwrap(), vec![]);
    assert!(s.next().is_none());
}

#[cfg(feature = "alloc")]
#[test]
fn read_line() {
    let mut buf = Cursor::new(&b"12"[..]);
    let mut v = String::new();
    assert_eq!(buf.read_line(&mut v).unwrap(), 2);
    assert_eq!(v, "12");

    let mut buf = Cursor::new(&b"12\n\n"[..]);
    let mut v = String::new();
    assert_eq!(buf.read_line(&mut v).unwrap(), 3);
    assert_eq!(v, "12\n");
    v.truncate(0);
    assert_eq!(buf.read_line(&mut v).unwrap(), 1);
    assert_eq!(v, "\n");
    v.truncate(0);
    assert_eq!(buf.read_line(&mut v).unwrap(), 0);
    assert_eq!(v, "");
}

#[cfg(feature = "alloc")]
#[test]
fn lines() {
    let buf = Cursor::new(&b"12\r"[..]);
    let mut s = buf.lines();
    assert_eq!(s.next().unwrap().unwrap(), "12\r".to_string());
    assert!(s.next().is_none());

    let buf = Cursor::new(&b"12\r\n\n"[..]);
    let mut s = buf.lines();
    assert_eq!(s.next().unwrap().unwrap(), "12".to_string());
    assert_eq!(s.next().unwrap().unwrap(), "".to_string());
    assert!(s.next().is_none());
}

#[test]
fn buf_read_has_data_left() {
    let mut buf = Cursor::new(&b"abcd"[..]);
    assert!(buf.has_data_left().unwrap());
    buf.read_exact(&mut [0; 2]).unwrap();
    assert!(buf.has_data_left().unwrap());
    buf.read_exact(&mut [0; 2]).unwrap();
    assert!(!buf.has_data_left().unwrap());
}

#[cfg(feature = "alloc")]
#[test]
fn read_to_end() {
    let mut c = Cursor::new(&b""[..]);
    let mut v = Vec::new();
    assert_eq!(c.read_to_end(&mut v).unwrap(), 0);
    assert_eq!(v, []);

    let mut c = Cursor::new(&b"1"[..]);
    let mut v = Vec::new();
    assert_eq!(c.read_to_end(&mut v).unwrap(), 1);
    assert_eq!(v, b"1");

    let cap = if cfg!(miri) { 1024 } else { 1024 * 1024 };
    let data = (0..cap).map(|i| (i / 3) as u8).collect::<Vec<_>>();
    let mut v = Vec::new();
    let (a, b) = data.split_at(data.len() / 2);
    assert_eq!(Cursor::new(a).read_to_end(&mut v).unwrap(), a.len());
    assert_eq!(Cursor::new(b).read_to_end(&mut v).unwrap(), b.len());
    assert_eq!(v, data);
}

#[cfg(feature = "alloc")]
#[test]
fn read_to_string() {
    let mut c = Cursor::new(&b""[..]);
    let mut v = String::new();
    assert_eq!(c.read_to_string(&mut v).unwrap(), 0);
    assert_eq!(v, "");

    let mut c = Cursor::new(&b"1"[..]);
    let mut v = String::new();
    assert_eq!(c.read_to_string(&mut v).unwrap(), 1);
    assert_eq!(v, "1");

    let mut c = Cursor::new(&b"\xff"[..]);
    let mut v = String::new();
    assert!(c.read_to_string(&mut v).is_err());
}

#[test]
fn read_exact() {
    let mut buf = [0; 4];

    let mut c = Cursor::new(&b""[..]);
    assert_eq!(c.read_exact(&mut buf).unwrap_err(), Error::UnexpectedEof);

    let mut c = Cursor::new(&b"123"[..]).chain(Cursor::new(&b"456789"[..]));
    c.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"1234");
    c.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"5678");
    assert_eq!(c.read_exact(&mut buf).unwrap_err(), Error::UnexpectedEof);
}

#[test]
fn read_exact_slice() {
    let mut buf = [0; 4];

    let mut c = &b""[..];
    assert_eq!(c.read_exact(&mut buf).unwrap_err(), Error::UnexpectedEof);

    let mut c = &b"123"[..];
    assert_eq!(c.read_exact(&mut buf).unwrap_err(), Error::UnexpectedEof);
    // make sure the optimized (early returning) method is being used
    assert_eq!(&buf, &[0; 4]);

    let mut c = &b"1234"[..];
    c.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"1234");

    let mut c = &b"56789"[..];
    c.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"5678");
    assert_eq!(c, b"9");
}

#[test]
fn read_buf_exact() {
    let buf: &mut [_] = &mut [0; 4];
    let mut buf: BorrowedBuf<'_> = buf.into();

    let mut c = Cursor::new(&b""[..]);
    assert_eq!(
        c.read_buf_exact(buf.unfilled()).unwrap_err(),
        Error::UnexpectedEof
    );

    let mut c = Cursor::new(&b"123456789"[..]);
    c.read_buf_exact(buf.unfilled()).unwrap();
    assert_eq!(buf.filled(), b"1234");

    buf.clear();

    c.read_buf_exact(buf.unfilled()).unwrap();
    assert_eq!(buf.filled(), b"5678");

    buf.clear();

    assert_eq!(
        c.read_buf_exact(buf.unfilled()).unwrap_err(),
        Error::UnexpectedEof
    );
}

#[test]
fn take_eof() {
    struct R;

    impl Read for R {
        fn read(&mut self, _: &mut [u8]) -> Result<usize> {
            Err(Error::Io)
        }
    }
    impl BufRead for R {
        fn fill_buf(&mut self) -> Result<&[u8]> {
            Err(Error::Io)
        }

        fn consume(&mut self, _amt: usize) {}
    }

    let mut buf = [0; 1];
    assert_eq!(0, R.take(0).read(&mut buf).unwrap());
    assert_eq!(b"", R.take(0).fill_buf().unwrap());
}

fn cmp_bufread<Br1: BufRead, Br2: BufRead>(mut br1: Br1, mut br2: Br2, exp: &[u8]) {
    let mut cat = Vec::new();
    loop {
        let consume = {
            let buf1 = br1.fill_buf().unwrap();
            let buf2 = br2.fill_buf().unwrap();
            let minlen = if buf1.len() < buf2.len() {
                buf1.len()
            } else {
                buf2.len()
            };
            assert_eq!(buf1[..minlen], buf2[..minlen]);
            cat.extend_from_slice(&buf1[..minlen]);
            minlen
        };
        if consume == 0 {
            break;
        }
        br1.consume(consume);
        br2.consume(consume);
    }
    assert_eq!(br1.fill_buf().unwrap().len(), 0);
    assert_eq!(br2.fill_buf().unwrap().len(), 0);
    assert_eq!(&cat[..], exp)
}

#[test]
fn chain_bufread() {
    let testdata = b"ABCDEFGHIJKL";
    let chain1 = (&testdata[..3])
        .chain(&testdata[3..6])
        .chain(&testdata[6..9])
        .chain(&testdata[9..]);
    let chain2 = (&testdata[..4])
        .chain(&testdata[4..8])
        .chain(&testdata[8..]);
    cmp_bufread(chain1, chain2, &testdata[..]);
}

#[cfg(feature = "alloc")]
#[test]
fn chain_splitted_char() {
    let chain = b"\xc3".chain(b"\xa9".as_slice());
    assert_eq!(axio::read_to_string(chain).unwrap(), "é");

    let mut chain = b"\xc3".chain(b"\xa9\n".as_slice());
    let mut buf = String::new();
    assert_eq!(chain.read_line(&mut buf).unwrap(), 3);
    assert_eq!(buf, "é\n");
}

#[cfg(feature = "alloc")]
#[test]
fn chain_zero_length_read_is_not_eof() {
    let a = b"A";
    let b = b"B";
    let mut s = String::new();
    let mut chain = (&a[..]).chain(&b[..]);
    chain.read(&mut []).unwrap();
    chain.read_to_string(&mut s).unwrap();
    assert_eq!("AB", s);
}

#[cfg(feature = "alloc")]
#[bench]
#[cfg_attr(miri, ignore)] // Miri isn't fast...
fn bench_read_to_end(b: &mut test::Bencher) {
    b.iter(|| {
        let mut lr = axio::repeat(1).take(10000000);
        let mut vec = Vec::with_capacity(1024);
        axio::default_read_to_end(&mut lr, &mut vec, None)
    });
}

#[test]
fn seek_len() -> Result<()> {
    let mut c = Cursor::new(vec![0; 15]);
    assert_eq!(c.stream_len()?, 15);

    c.seek(SeekFrom::End(0))?;
    let old_pos = c.stream_position()?;
    assert_eq!(c.stream_len()?, 15);
    assert_eq!(c.stream_position()?, old_pos);

    c.seek(SeekFrom::Start(7))?;
    c.seek(SeekFrom::Current(2))?;
    let old_pos = c.stream_position()?;
    assert_eq!(c.stream_len()?, 15);
    assert_eq!(c.stream_position()?, old_pos);

    Ok(())
}

#[test]
fn seek_position() -> Result<()> {
    // All `asserts` are duplicated here to make sure the method does not
    // change anything about the seek state.
    let mut c = Cursor::new(vec![0; 15]);
    assert_eq!(c.stream_position()?, 0);
    assert_eq!(c.stream_position()?, 0);

    c.seek(SeekFrom::End(0))?;
    assert_eq!(c.stream_position()?, 15);
    assert_eq!(c.stream_position()?, 15);

    c.seek(SeekFrom::Start(7))?;
    c.seek(SeekFrom::Current(2))?;
    assert_eq!(c.stream_position()?, 9);
    assert_eq!(c.stream_position()?, 9);

    c.seek(SeekFrom::End(-3))?;
    c.seek(SeekFrom::Current(1))?;
    c.seek(SeekFrom::Current(-5))?;
    assert_eq!(c.stream_position()?, 8);
    assert_eq!(c.stream_position()?, 8);

    c.rewind()?;
    assert_eq!(c.stream_position()?, 0);
    assert_eq!(c.stream_position()?, 0);

    Ok(())
}

#[test]
fn take_seek() -> Result<()> {
    let mut buf = Cursor::new(b"0123456789");
    buf.set_position(2);
    let mut take = buf.by_ref().take(4);
    let mut buf1 = [0u8; 1];
    let mut buf2 = [0u8; 2];
    assert_eq!(take.position(), 0);

    assert_eq!(take.seek(SeekFrom::Start(0))?, 0);
    take.read_exact(&mut buf2)?;
    assert_eq!(buf2, [b'2', b'3']);
    assert_eq!(take.seek(SeekFrom::Start(1))?, 1);
    take.read_exact(&mut buf2)?;
    assert_eq!(buf2, [b'3', b'4']);
    assert_eq!(take.seek(SeekFrom::Start(2))?, 2);
    take.read_exact(&mut buf2)?;
    assert_eq!(buf2, [b'4', b'5']);
    assert_eq!(take.seek(SeekFrom::Start(3))?, 3);
    take.read_exact(&mut buf1)?;
    assert_eq!(buf1, [b'5']);
    assert_eq!(take.seek(SeekFrom::Start(4))?, 4);
    assert_eq!(take.read(&mut buf1)?, 0);

    assert_eq!(take.seek(SeekFrom::End(0))?, 4);
    assert_eq!(take.seek(SeekFrom::End(-1))?, 3);
    take.read_exact(&mut buf1)?;
    assert_eq!(buf1, [b'5']);
    assert_eq!(take.seek(SeekFrom::End(-2))?, 2);
    take.read_exact(&mut buf2)?;
    assert_eq!(buf2, [b'4', b'5']);
    assert_eq!(take.seek(SeekFrom::End(-3))?, 1);
    take.read_exact(&mut buf2)?;
    assert_eq!(buf2, [b'3', b'4']);
    assert_eq!(take.seek(SeekFrom::End(-4))?, 0);
    take.read_exact(&mut buf2)?;
    assert_eq!(buf2, [b'2', b'3']);

    assert_eq!(take.seek(SeekFrom::Current(0))?, 2);
    take.read_exact(&mut buf2)?;
    assert_eq!(buf2, [b'4', b'5']);

    assert_eq!(take.seek(SeekFrom::Current(-3))?, 1);
    take.read_exact(&mut buf2)?;
    assert_eq!(buf2, [b'3', b'4']);

    assert_eq!(take.seek(SeekFrom::Current(-1))?, 2);
    take.read_exact(&mut buf2)?;
    assert_eq!(buf2, [b'4', b'5']);

    assert_eq!(take.seek(SeekFrom::Current(-4))?, 0);
    take.read_exact(&mut buf2)?;
    assert_eq!(buf2, [b'2', b'3']);

    assert_eq!(take.seek(SeekFrom::Current(2))?, 4);
    assert_eq!(take.read(&mut buf1)?, 0);

    Ok(())
}

#[test]
fn take_seek_error() {
    let buf = Cursor::new(b"0123456789");
    let mut take = buf.take(2);
    assert!(take.seek(SeekFrom::Start(3)).is_err());
    assert!(take.seek(SeekFrom::End(1)).is_err());
    assert!(take.seek(SeekFrom::End(-3)).is_err());
    assert!(take.seek(SeekFrom::Current(-1)).is_err());
    assert!(take.seek(SeekFrom::Current(3)).is_err());
}

struct ExampleHugeRangeOfZeroes {
    position: u64,
}

impl Read for ExampleHugeRangeOfZeroes {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let max = buf.len().min(usize::MAX);
        for (i, e) in buf.iter_mut().enumerate().take(max) {
            if self.position == u64::MAX {
                return Ok(i);
            }
            self.position += 1;
            *e = 0;
        }
        Ok(max)
    }
}

impl Seek for ExampleHugeRangeOfZeroes {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
        match pos {
            SeekFrom::Start(i) => self.position = i,
            SeekFrom::End(i) if i >= 0 => self.position = u64::MAX,
            SeekFrom::End(i) => self.position -= i.unsigned_abs(),
            SeekFrom::Current(i) => {
                self.position = if i >= 0 {
                    self.position.saturating_add(i.unsigned_abs())
                } else {
                    self.position.saturating_sub(i.unsigned_abs())
                };
            }
        }
        Ok(self.position)
    }
}

#[test]
fn take_seek_big_offsets() -> Result<()> {
    let inner = ExampleHugeRangeOfZeroes { position: 1 };
    let mut take = inner.take(u64::MAX - 2);
    assert_eq!(take.seek(SeekFrom::Start(u64::MAX - 2))?, u64::MAX - 2);
    assert_eq!(take.get_ref().position, u64::MAX - 1);
    assert_eq!(take.seek(SeekFrom::Start(0))?, 0);
    assert_eq!(take.get_ref().position, 1);
    assert_eq!(take.seek(SeekFrom::End(-1))?, u64::MAX - 3);
    assert_eq!(take.get_ref().position, u64::MAX - 2);
    Ok(())
}

// A simple example reader which uses the default implementation of
// read_to_end.
#[cfg(feature = "alloc")]
struct ExampleSliceReader<'a> {
    slice: &'a [u8],
}

#[cfg(feature = "alloc")]
impl<'a> Read for ExampleSliceReader<'a> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let len = core::cmp::min(self.slice.len(), buf.len());
        buf[..len].copy_from_slice(&self.slice[..len]);
        self.slice = &self.slice[len..];
        Ok(len)
    }
}

#[cfg(feature = "alloc")]
#[test]
fn test_read_to_end_capacity() -> Result<()> {
    let input = &b"foo"[..];

    // read_to_end() takes care not to over-allocate when a buffer is the
    // exact size needed.
    let mut vec1 = Vec::with_capacity(input.len());
    ExampleSliceReader { slice: input }.read_to_end(&mut vec1)?;
    assert_eq!(vec1.len(), input.len());
    assert_eq!(vec1.capacity(), input.len(), "did not allocate more");

    Ok(())
}

// Issue 94981
#[test]
#[should_panic = "number of read bytes exceeds limit"]
fn test_take_wrong_length() {
    struct LieAboutSize(bool);

    impl Read for LieAboutSize {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
            // Lie about the read size at first time of read.
            if core::mem::take(&mut self.0) {
                Ok(buf.len() + 1)
            } else {
                Ok(buf.len())
            }
        }
    }

    let mut buffer = [0; 4];
    let mut reader = LieAboutSize(true).take(4);
    // Primed the `Limit` by lying about the read size.
    let _ = reader.read(&mut buffer[..]);
}

#[test]
fn slice_read_exact_eof() {
    let slice = &b"123456"[..];

    let mut r = slice;
    assert!(r.read_exact(&mut [0; 10]).is_err());
    assert!(r.is_empty());

    let mut r = slice;
    let buf = &mut [0; 10];
    let mut buf = BorrowedBuf::from(buf.as_mut_slice());
    assert!(r.read_buf_exact(buf.unfilled()).is_err());
    assert!(r.is_empty());
    assert_eq!(buf.filled(), b"123456");
}

#[test]
fn cursor_read_exact_eof() {
    let slice = Cursor::new(b"123456");

    let mut r = slice.clone();
    assert!(r.read_exact(&mut [0; 10]).is_err());
    assert!(Cursor::split(&r).1.is_empty());

    let mut r = slice;
    let buf = &mut [0; 10];
    let mut buf = BorrowedBuf::from(buf.as_mut_slice());
    assert!(r.read_buf_exact(buf.unfilled()).is_err());
    assert!(Cursor::split(&r).1.is_empty());
    assert_eq!(buf.filled(), b"123456");
}

#[bench]
fn bench_take_read(b: &mut test::Bencher) {
    b.iter(|| {
        let mut buf = [0; 64];

        [255; 128].take(64).read(&mut buf).unwrap();
    });
}

#[bench]
fn bench_take_read_buf(b: &mut test::Bencher) {
    b.iter(|| {
        let buf: &mut [_] = &mut [MaybeUninit::uninit(); 64];

        let mut buf: BorrowedBuf<'_> = buf.into();

        [255; 128].take(64).read_buf(buf.unfilled()).unwrap();
    });
}

// Issue #120603
#[test]
#[should_panic]
fn read_buf_broken_read() {
    struct MalformedRead;

    impl Read for MalformedRead {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
            // broken length calculation
            Ok(buf.len() + 1)
        }
    }

    let _ = BufReader::new(MalformedRead).fill_buf();
}

#[test]
fn read_buf_full_read() {
    struct FullRead;

    impl Read for FullRead {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
            Ok(buf.len())
        }
    }

    assert_eq!(
        BufReader::new(FullRead).fill_buf().unwrap().len(),
        DEFAULT_BUF_SIZE
    );
}

struct DataAndErrorReader(&'static [u8]);

impl Read for DataAndErrorReader {
    fn read(&mut self, _buf: &mut [u8]) -> Result<usize> {
        panic!("We want tests to use `read_buf`")
    }

    fn read_buf(&mut self, buf: BorrowedCursor<'_>) -> Result<()> {
        self.0.read_buf(buf).unwrap();
        Err(Error::Io)
    }
}

#[test]
fn read_buf_data_and_error_take() {
    let mut buf = [0; 64];
    let mut buf = BorrowedBuf::from(buf.as_mut_slice());

    let mut r = DataAndErrorReader(&[4, 5, 6]).take(1);
    assert!(r.read_buf(buf.unfilled()).is_err());
    assert_eq!(buf.filled(), &[4]);

    assert!(r.read_buf(buf.unfilled()).is_ok());
    assert_eq!(buf.filled(), &[4]);
    assert_eq!(r.get_ref().0, &[5, 6]);
}

#[test]
fn read_buf_data_and_error_buf() {
    let mut r = BufReader::new(DataAndErrorReader(&[4, 5, 6]));

    assert!(r.fill_buf().is_err());
    assert_eq!(r.fill_buf().unwrap(), &[4, 5, 6]);
}

#[cfg(feature = "alloc")]
#[test]
fn read_buf_data_and_error_read_to_end() {
    let mut r = DataAndErrorReader(&[4, 5, 6]);

    let mut v = Vec::with_capacity(200);
    assert!(r.read_to_end(&mut v).is_err());

    assert_eq!(v, &[4, 5, 6]);
}

#[cfg(feature = "alloc")]
#[test]
fn read_to_end_error() {
    struct ErrorReader;

    impl Read for ErrorReader {
        fn read(&mut self, _buf: &mut [u8]) -> Result<usize> {
            Err(Error::Io)
        }
    }

    let mut r = [4, 5, 6].chain(ErrorReader);

    let mut v = Vec::with_capacity(200);
    assert!(r.read_to_end(&mut v).is_err());

    assert_eq!(v, &[4, 5, 6]);
}
