use axio::{BufReader, BufWriter, Cursor, SeekFrom, empty, prelude::*, repeat, sink};

#[test]
fn test_slice() {
    let mut buf: &[u8] = &[1, 2, 3, 4, 5];
    assert_eq!(buf.remaining(), 5);

    buf.read(&mut [0; 2]).unwrap();
    assert_eq!(buf.remaining(), 3);

    let mut buf: &mut [u8] = &mut [0; 10];
    assert_eq!(buf.remaining_mut(), 10);

    buf.write(&[1, 2, 3, 4, 5]).unwrap();
    assert_eq!(buf.remaining_mut(), 5);
}

#[cfg(feature = "alloc")]
#[test]
fn test_vec() {
    let buf: Vec<u8> = vec![1, 2, 3, 4, 5];
    assert_eq!(buf.remaining(), 5);
    assert_eq!(buf.remaining_mut(), (isize::MAX as usize) - 5);

    let buf: &mut Vec<u8> = &mut vec![0; 10];
    assert_eq!(buf.remaining(), 10);

    let buf: Box<Vec<u8>> = Box::new(vec![1, 2, 3]);
    assert_eq!(buf.remaining(), 3);
    assert_eq!(buf.remaining_mut(), (isize::MAX as usize) - 3);
}

#[test]
fn test_chain() {
    let buf1: &[u8] = &[1, 2, 3];
    let buf2: &[u8] = &[4, 5];
    let mut chain = buf1.chain(buf2);
    assert_eq!(chain.remaining(), 5);

    chain.read(&mut [0; 2]).unwrap();
    assert_eq!(chain.remaining(), 3);

    chain.read(&mut [0; 1]).unwrap();
    assert_eq!(chain.remaining(), 2);

    chain.read(&mut [0; 2]).unwrap();
    assert_eq!(chain.remaining(), 0);
}

#[test]
fn test_cursor() {
    let data: &[u8] = &[1, 2, 3, 4, 5];
    let mut cursor = Cursor::new(data);

    assert_eq!(cursor.remaining(), 5);
    cursor.read(&mut [0; 2]).unwrap();
    assert_eq!(cursor.remaining(), 3);

    cursor.seek(SeekFrom::Start(0)).unwrap();
    assert_eq!(cursor.remaining(), 5);

    cursor.seek(SeekFrom::End(-2)).unwrap();
    assert_eq!(cursor.remaining(), 2);
}

#[test]
fn test_empty() {
    let mut empty = empty();
    assert_eq!(empty.remaining(), 0);
    assert_eq!(empty.remaining_mut(), usize::MAX);

    empty.read(&mut [0; 10]).unwrap();
    assert_eq!(empty.remaining(), 0);

    empty.write(&[1, 2, 3]).unwrap();
    assert_eq!(empty.remaining_mut(), usize::MAX);
}

#[test]
fn test_repeat() {
    let mut repeat = repeat(0u8);
    assert_eq!(repeat.remaining(), usize::MAX);

    repeat.read(&mut [0; 10]).unwrap();
    assert_eq!(repeat.remaining(), usize::MAX);
}

#[test]
fn test_sink() {
    let mut sink = sink();
    assert_eq!(sink.remaining_mut(), usize::MAX);

    sink.write(&[1, 2, 3, 4, 5]).unwrap();
    assert_eq!(sink.remaining_mut(), usize::MAX);
}

#[test]
fn test_take() {
    let data: &[u8] = &[1, 2, 3, 4, 5];
    let mut take = data.take(3);
    assert_eq!(take.remaining(), 3);

    take.read(&mut [0; 2]).unwrap();
    assert_eq!(take.remaining(), 1);

    take.read(&mut [0; 2]).unwrap();
    assert_eq!(take.remaining(), 0);

    let data: &[u8] = &[];
    let take = data.take(3);
    assert_eq!(take.remaining(), 0);
}

#[test]
fn test_bufreader() {
    let data: &[u8] = &[1, 2, 3, 4, 5];
    let mut reader = BufReader::with_capacity(2, data);
    assert_eq!(reader.remaining(), 5);

    reader.read(&mut [0; 1]).unwrap();
    assert_eq!(reader.remaining(), 4);

    assert_eq!(reader.read(&mut [0; 2]).unwrap(), 1);
    assert_eq!(reader.remaining(), 3);
}

#[test]
fn test_bufwriter() {
    let data: &mut [u8] = &mut [0; 5];
    let mut writer = BufWriter::new(data);
    assert_eq!(writer.remaining_mut(), 5);

    writer.write(&[1, 2, 3]).unwrap();
    assert_eq!(writer.remaining_mut(), 2);

    writer.write(&[4, 5, 6]).unwrap();
    assert_eq!(writer.remaining_mut(), 0);
}

#[test]
fn test_strict_write_to() {
    struct StrictWriter {
        rest: usize,
    }

    impl Write for StrictWriter {
        fn write(&mut self, buf: &[u8]) -> axio::Result<usize> {
            assert!(buf.len() <= self.rest);
            let to_write = buf.len();
            self.rest -= to_write;
            Ok(to_write)
        }

        fn flush(&mut self) -> axio::Result<()> {
            Ok(())
        }
    }

    let mut writer = StrictWriter { rest: 5 };
    let mut buf: &[u8] = &[0; 5];

    assert_eq!(buf.write_to(&mut writer).unwrap(), 5);

    std::panic::catch_unwind(move || {
        let mut writer = StrictWriter { rest: 3 };
        let mut buf: &[u8] = &[0; 5];
        buf.write_to(&mut writer).unwrap();
    })
    .unwrap_err();
}

#[test]
fn test_strict_read_from() {
    struct StrictReader {
        rest: usize,
    }

    impl Read for StrictReader {
        fn read(&mut self, buf: &mut [u8]) -> axio::Result<usize> {
            assert!(buf.len() <= self.rest);
            let to_read = buf.len();
            self.rest -= to_read;
            for b in &mut buf[..to_read] {
                *b = 0;
            }
            Ok(to_read)
        }
    }

    let mut reader = StrictReader { rest: 5 };
    let mut buf: &mut [u8] = &mut [0; 5];

    assert_eq!(buf.read_from(&mut reader).unwrap(), 5);

    std::panic::catch_unwind(move || {
        let mut reader = StrictReader { rest: 3 };
        let mut buf: &mut [u8] = &mut [0; 5];
        buf.read_from(&mut reader).unwrap();
    })
    .unwrap_err();
}

#[test]
fn test_small_read_from() {
    struct SmallReader;

    impl Read for SmallReader {
        fn read(&mut self, buf: &mut [u8]) -> axio::Result<usize> {
            let to_read = std::cmp::min(buf.len(), 3);
            for b in &mut buf[..to_read] {
                *b = 0;
            }
            Ok(to_read)
        }
    }

    let mut reader = SmallReader;
    let mut buf: &mut [u8] = &mut [0; 5];

    assert_eq!(buf.read_from(&mut reader).unwrap(), 3);
}
