use alloc::{
    boxed::Box,
    collections::VecDeque,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};

use axtest::prelude::*;

#[axtest::def_test]
fn axio_slice_read_rules_hold() {
    use ax_io::{BufRead, Error, Read, read_to_string};

    let mut input: &[u8] = b"abcdef";
    let mut first = [0; 1];
    ax_assert_eq!(input.read(&mut first).unwrap(), 1);
    ax_assert_eq!(&first, b"a");

    let mut next = [0; 3];
    input.read_exact(&mut next).unwrap();
    ax_assert_eq!(&next, b"bcd");
    ax_assert_eq!(input, b"ef");

    let mut too_large = [0; 4];
    ax_assert_eq!(input.read_exact(&mut too_large), Err(Error::UnexpectedEof));
    ax_assert!(input.is_empty());

    let mut collected = vec![b'>'];
    let mut input: &[u8] = b"tail";
    ax_assert_eq!(input.read_to_end(&mut collected).unwrap(), 4);
    ax_assert_eq!(collected, b">tail");

    let mut text = "prefix:".to_string();
    let mut input: &[u8] = "ok".as_bytes();
    ax_assert_eq!(input.read_to_string(&mut text).unwrap(), 2);
    ax_assert_eq!(text, "prefix:ok");

    let mut bad_utf8: &[u8] = &[0xff];
    let mut text = String::new();
    ax_assert_eq!(bad_utf8.read_to_string(&mut text), Err(Error::IllegalBytes));

    let whole = read_to_string("whole".as_bytes()).unwrap();
    ax_assert_eq!(whole, "whole");

    let mut buffered: &[u8] = b"alpha\nbeta";
    ax_assert!(buffered.has_data_left().unwrap());
    ax_assert_eq!(buffered.skip_until(b'\n').unwrap(), 6);
    let mut line = String::new();
    ax_assert_eq!(buffered.read_line(&mut line).unwrap(), 4);
    ax_assert_eq!(line, "beta");
    ax_assert!(!buffered.has_data_left().unwrap());
}

#[axtest::def_test]
fn axio_adapter_read_rules_hold() {
    use ax_io::{Error, Read};

    let mut chained = (&b"ab"[..]).chain(&b"cd"[..]);
    let mut all = Vec::new();
    ax_assert_eq!(chained.read_to_end(&mut all).unwrap(), 4);
    ax_assert_eq!(all, b"abcd");

    let mut limited = (&b"abcdef"[..]).take(3);
    let mut clipped = Vec::new();
    ax_assert_eq!(limited.read_to_end(&mut clipped).unwrap(), 3);
    ax_assert_eq!(clipped, b"abc");

    let mut inner: &[u8] = b"boxed";
    let mut boxed: Box<&mut dyn Read> = Box::new(&mut inner);
    let mut word = [0; 5];
    boxed.read_exact(&mut word).unwrap();
    ax_assert_eq!(&word, b"boxed");

    let mut deque = VecDeque::from(Vec::from(&b"front-back"[..]));
    let mut front = [0; 5];
    deque.read_exact(&mut front).unwrap();
    ax_assert_eq!(&front, b"front");
    ax_assert_eq!(deque.len(), 5);
    let mut rest = Vec::new();
    ax_assert_eq!(deque.read_to_end(&mut rest).unwrap(), 5);
    ax_assert_eq!(rest, b"-back");

    let mut short = VecDeque::from(Vec::from(&b"xy"[..]));
    let mut too_large = [0; 3];
    ax_assert_eq!(short.read_exact(&mut too_large), Err(Error::UnexpectedEof));
    ax_assert!(short.is_empty());
}

#[axtest::def_test]
fn axio_write_rules_hold() {
    use ax_io::{Error, Write};

    let mut storage = [0; 5];
    {
        let mut writer = &mut storage[..];
        ax_assert_eq!(writer.write(b"abc").unwrap(), 3);
        ax_assert_eq!(writer.write_all(b"de"), Ok(()));
        ax_assert_eq!(writer.write_all(b"!"), Err(Error::WriteZero));
        ax_assert_eq!(writer.flush(), Ok(()));
    }
    ax_assert_eq!(&storage, b"abcde");

    let mut vec_writer = Vec::new();
    vec_writer.write_all(b"hello").unwrap();
    vec_writer.write_fmt(format_args!(" {}", 42)).unwrap();
    ax_assert_eq!(vec_writer, b"hello 42");

    let mut deque = VecDeque::new();
    deque.write_all(b"abc").unwrap();
    ax_assert_eq!(deque.len(), 3);
    ax_assert_eq!(deque.pop_front(), Some(b'a'));

    let mut boxed: Box<dyn Write> = Box::new(Vec::<u8>::new());
    boxed.write_all(b"through-box").unwrap();
    boxed.flush().unwrap();
}

#[axtest::def_test]
fn axio_buffered_reader_rules_hold() {
    use ax_io::{BufRead, BufReader, IoBuf, Read};

    let mut reader = BufReader::with_capacity(4, &b"ab\ncd\nef"[..]);
    ax_assert_eq!(reader.capacity(), 4);
    ax_assert_eq!(reader.peek(2).unwrap(), b"ab");
    ax_assert_eq!(reader.buffer(), b"ab\nc");
    ax_assert_eq!(reader.remaining(), 8);

    let mut line = String::new();
    ax_assert_eq!(reader.read_line(&mut line).unwrap(), 3);
    ax_assert_eq!(line, "ab\n");
    ax_assert_eq!(reader.peek(2).unwrap(), b"cd");
    reader.consume(1);

    let mut bytes = Vec::new();
    ax_assert_eq!(reader.read_to_end(&mut bytes).unwrap(), 4);
    ax_assert_eq!(bytes, b"d\nef");

    let reader = BufReader::with_capacity(2, &b"x\ny\r\nz"[..]);
    let mut lines = reader.lines();
    ax_assert_eq!(lines.next().unwrap().unwrap(), "x");
    ax_assert_eq!(lines.next().unwrap().unwrap(), "y");
    ax_assert_eq!(lines.next().unwrap().unwrap(), "z");
    ax_assert!(lines.next().is_none());

    let reader = BufReader::with_capacity(2, &b"a,b,c"[..]);
    let fields: Vec<Vec<u8>> = reader.split(b',').map(|part| part.unwrap()).collect();
    ax_assert_eq!(fields, vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]);
}

#[axtest::def_test]
fn axio_buffered_writer_rules_hold() {
    use ax_io::{BufWriter, IoBufMut, Write};

    let inner = Vec::<u8>::new();
    let mut writer = BufWriter::with_capacity(4, inner);
    ax_assert_eq!(writer.capacity(), 4);
    ax_assert_eq!(writer.remaining_mut(), isize::MAX as usize);
    writer.write_all(b"ab").unwrap();
    ax_assert_eq!(writer.buffer(), b"ab");
    writer.write_all(b"cd").unwrap();
    ax_assert_eq!(writer.buffer(), b"abcd");
    writer.write_all(b"efg").unwrap();
    ax_assert_eq!(writer.buffer(), b"efg");
    writer.write_all(b"hij").unwrap();
    ax_assert_eq!(writer.buffer(), b"hij");
    writer.write_all(b"i").unwrap();
    ax_assert_eq!(writer.buffer(), b"hiji");
    writer.flush().unwrap();
    let inner = writer.into_inner().unwrap();
    ax_assert_eq!(inner, b"abcdefghiji");

    let writer = BufWriter::with_capacity(8, Vec::<u8>::new());
    let (inner, buffered) = writer.into_parts();
    ax_assert!(inner.is_empty());
    ax_assert!(buffered.unwrap().is_empty());
}

#[axtest::def_test]
fn axio_iobuf_extension_rules_hold() {
    use ax_io::{BufReader, IoBuf, IoBufExt, IoBufMut, IoBufMutExt, Read};

    let mut input: &[u8] = b"copy";
    ax_assert_eq!(input.remaining(), 4);
    ax_assert!(!input.is_empty());
    let mut output = Vec::new();
    ax_assert_eq!(input.write_to(&mut output).unwrap(), 4);
    ax_assert_eq!(output, b"copy");
    ax_assert_eq!(input, b"copy");

    let mut fixed = [0; 3];
    {
        let mut writer = &mut fixed[..];
        let mut reader: &[u8] = b"abcdef";
        ax_assert_eq!(writer.remaining_mut(), 3);
        ax_assert_eq!(writer.read_from(&mut reader).unwrap(), 3);
        ax_assert_eq!(writer.remaining_mut(), 3);
        ax_assert_eq!(reader, b"def");
    }
    ax_assert_eq!(&fixed, b"abc");

    let mut growable = Vec::with_capacity(8);
    let mut reader: &[u8] = b"grow";
    ax_assert_eq!(growable.read_from(&mut reader).unwrap(), 4);
    ax_assert_eq!(growable, b"grow");

    let mut buffered = BufReader::with_capacity(2, &b"xyzw"[..]);
    let mut sink = Vec::new();
    ax_assert_eq!(buffered.write_to(&mut sink).unwrap(), 2);
    ax_assert_eq!(sink, b"xy");
    let mut rest = Vec::new();
    buffered.read_to_end(&mut rest).unwrap();
    ax_assert_eq!(rest, b"zw");
}

#[axtest::def_test]
fn axio_poll_state_and_formatting_rules_hold() {
    use ax_io::PollState;

    let default_state = PollState::default();
    ax_assert!(!default_state.readable);
    ax_assert!(!default_state.writable);
    ax_assert_eq!(default_state.readiness_version, 0);

    let state = PollState {
        readable: true,
        writable: true,
        readiness_version: 7,
    };
    let formatted = format!("{state:?}");
    ax_assert!(formatted.contains("readable: true"));
    ax_assert!(formatted.contains("readiness_version: 7"));
}

#[axtest::def_test]
fn axio_cursor_seek_read_write_rules_hold() {
    use ax_io::{Cursor, Error, IoBuf, IoBufMut, Read, Seek, SeekFrom, Write};

    let mut cursor = Cursor::new(Vec::from(&b"abcdef"[..]));
    ax_assert_eq!(cursor.position(), 0);
    ax_assert_eq!(cursor.stream_len().unwrap(), 6);
    ax_assert_eq!(cursor.seek(SeekFrom::Start(2)).unwrap(), 2);
    ax_assert_eq!(cursor.stream_position().unwrap(), 2);
    ax_assert_eq!(cursor.split(), (&b"ab"[..], &b"cdef"[..]));

    let mut read = [0; 2];
    cursor.read_exact(&mut read).unwrap();
    ax_assert_eq!(&read, b"cd");
    ax_assert_eq!(cursor.position(), 4);
    ax_assert_eq!(cursor.seek(SeekFrom::Current(-1)).unwrap(), 3);
    ax_assert_eq!(cursor.seek(SeekFrom::End(-1)).unwrap(), 5);
    ax_assert_eq!(
        cursor.seek(SeekFrom::Current(-10)),
        Err(Error::InvalidInput)
    );
    cursor.rewind().unwrap();
    cursor.seek_relative(1).unwrap();
    ax_assert_eq!(cursor.position(), 1);

    let mut text = String::new();
    cursor.read_to_string(&mut text).unwrap();
    ax_assert_eq!(text, "bcdef");

    let mut storage = [0; 4];
    let mut fixed = Cursor::new(&mut storage[..]);
    fixed.write_all(b"ab").unwrap();
    ax_assert_eq!(fixed.remaining_mut(), 2);
    ax_assert_eq!(fixed.write_all(b"cde"), Err(Error::WriteZero));
    ax_assert_eq!(&storage, b"abcd");

    let mut grow = Cursor::new(Vec::from(&b"hello"[..]));
    grow.write_all(b"HE").unwrap();
    grow.seek(SeekFrom::Start(7)).unwrap();
    grow.write_all(b"!").unwrap();
    ax_assert_eq!(grow.into_inner(), b"HEllo\0\0!");

    let mut boxed = Cursor::new(Vec::from(&b"box"[..]));
    let mut boxed_seek: Box<&mut dyn Seek> = Box::new(&mut boxed);
    ax_assert_eq!(boxed_seek.seek(SeekFrom::End(0)).unwrap(), 3);

    let source = Cursor::new(&b"remain"[..]);
    ax_assert_eq!(source.remaining(), 6);
}

#[axtest::def_test]
fn axio_empty_repeat_sink_rules_hold() {
    use ax_io::{
        BufRead, Error, IoBuf, IoBufMut, Read, Seek, SeekFrom, Write, empty, repeat, sink,
    };

    let mut empty = empty();
    let mut byte = [0; 1];
    ax_assert_eq!(empty.read(&mut byte).unwrap(), 0);
    ax_assert_eq!(empty.read_exact(&mut byte), Err(Error::UnexpectedEof));
    ax_assert_eq!(empty.fill_buf().unwrap(), b"");
    ax_assert_eq!(empty.skip_until(b'\n').unwrap(), 0);
    ax_assert!(!empty.has_data_left().unwrap());
    ax_assert_eq!(empty.seek(SeekFrom::End(99)).unwrap(), 0);
    ax_assert_eq!(empty.stream_len().unwrap(), 0);
    ax_assert_eq!(empty.write(b"ignored").unwrap(), 7);
    empty.write_fmt(format_args!("{}", "also ignored")).unwrap();
    ax_assert_eq!(empty.remaining(), 0);
    ax_assert_eq!(empty.remaining_mut(), usize::MAX);

    let mut repeat = repeat(0x5a);
    let mut filled = [0; 4];
    repeat.read_exact(&mut filled).unwrap();
    ax_assert_eq!(&filled, &[0x5a; 4]);
    ax_assert_eq!(repeat.remaining(), usize::MAX);
    let mut out = Vec::new();
    ax_assert_eq!(repeat.read_to_end(&mut out), Err(Error::NoMemory));

    let mut sink = sink();
    ax_assert_eq!(sink.write(b"abc").unwrap(), 3);
    sink.write_all(b"def").unwrap();
    sink.write_fmt(format_args!(" {}", 1)).unwrap();
    sink.flush().unwrap();
    ax_assert_eq!(sink.remaining_mut(), usize::MAX);
    let mut sink_ref = &sink;
    ax_assert_eq!(sink_ref.write(b"via-ref").unwrap(), 7);
}

#[axtest::def_test]
fn axio_copy_and_function_adapter_rules_hold() {
    use ax_io::{BufReader, BufWriter, Read, Write, copy, read_fn, stack_buffer_copy, write_fn};

    let mut source: &[u8] = b"slice-copy";
    let mut target = Vec::new();
    ax_assert_eq!(copy(&mut source, &mut target).unwrap(), 10);
    ax_assert_eq!(target, b"slice-copy");
    ax_assert!(source.is_empty());

    let mut deque = VecDeque::from(Vec::from(&b"deque-copy"[..]));
    let mut target = Vec::new();
    ax_assert_eq!(copy(&mut deque, &mut target).unwrap(), 10);
    ax_assert_eq!(target, b"deque-copy");
    ax_assert!(deque.is_empty());

    let mut reader = BufReader::with_capacity(4, &b"buffered-reader"[..]);
    let mut target = Vec::new();
    ax_assert_eq!(copy(&mut reader, &mut target).unwrap(), 15);
    ax_assert_eq!(target, b"buffered-reader");

    let inner = Vec::new();
    let mut writer = BufWriter::with_capacity(4096, inner);
    let mut source: &[u8] = b"buffered-writer";
    ax_assert_eq!(copy(&mut source, &mut writer).unwrap(), 15);
    ax_assert_eq!(writer.into_inner().unwrap(), b"buffered-writer");

    let mut source: &[u8] = b"stack";
    let mut written = Vec::new();
    ax_assert_eq!(stack_buffer_copy(&mut source, &mut written).unwrap(), 5);
    ax_assert_eq!(written, b"stack");

    let mut calls = 0usize;
    let mut generated = read_fn(|buf: &mut [u8]| {
        if calls == 0 {
            buf[..3].copy_from_slice(b"abc");
            calls += 1;
            Ok(3)
        } else {
            Ok(0)
        }
    });
    let mut output = Vec::new();
    ax_assert_eq!(generated.read_to_end(&mut output).unwrap(), 3);
    ax_assert_eq!(output, b"abc");

    let mut captured = Vec::new();
    {
        let mut writer = write_fn(|buf: &[u8]| {
            captured.extend_from_slice(buf);
            Ok(buf.len())
        });
        writer.write_all(b"fn-writer").unwrap();
        writer.flush().unwrap();
    }
    ax_assert_eq!(captured, b"fn-writer");
}

#[axtest::def_test]
fn axio_line_writer_rules_hold() {
    use ax_io::{IoBufMut, LineWriter, Write};

    let inner = Vec::<u8>::new();
    let mut writer = LineWriter::with_capacity(8, inner);
    writer.write_all(b"partial").unwrap();
    ax_assert!(writer.get_ref().is_empty());
    writer.write_all(b"\nnext").unwrap();
    ax_assert_eq!(writer.get_ref(), b"partial\n");
    ax_assert!(writer.remaining_mut() > 0);
    writer.write_fmt(format_args!(" line {}\n", 2)).unwrap();
    ax_assert_eq!(writer.get_ref(), b"partial\nnext line 2\n");
    writer.write(b"tail").unwrap();
    ax_assert_eq!(writer.get_ref(), b"partial\nnext line 2\n");
    writer.flush().unwrap();
    ax_assert_eq!(writer.get_ref(), b"partial\nnext line 2\ntail");

    let inner = writer.into_inner().unwrap();
    ax_assert_eq!(inner, b"partial\nnext line 2\ntail");
}

#[axtest::def_test]
fn axio_take_chain_and_recovery_rules_hold() {
    use ax_io::{BufRead, BufWriter, Cursor, Error, Read, Seek, SeekFrom, Write};

    let mut limited = Cursor::new(Vec::from(&b"abcdef"[..])).take(4);
    ax_assert_eq!(limited.limit(), 4);
    ax_assert_eq!(limited.position(), 0);
    ax_assert_eq!(limited.stream_len().unwrap(), 4);

    let mut first = [0; 2];
    ax_assert_eq!(limited.read(&mut first).unwrap(), 2);
    ax_assert_eq!(&first, b"ab");
    ax_assert_eq!(limited.position(), 2);
    ax_assert_eq!(limited.limit(), 2);

    limited.seek(SeekFrom::Start(1)).unwrap();
    ax_assert_eq!(limited.position(), 1);
    ax_assert_eq!(limited.limit(), 3);
    limited.seek_relative(2).unwrap();
    ax_assert_eq!(limited.position(), 3);
    ax_assert_eq!(limited.seek_relative(2), Err(Error::InvalidInput));
    ax_assert_eq!(limited.get_ref().position(), 3);
    limited.get_mut().set_position(0);
    ax_assert_eq!(limited.into_inner().position(), 0);

    let mut limited_buf = (&b"line-one\nline-two"[..]).take(8);
    ax_assert_eq!(limited_buf.fill_buf().unwrap(), b"line-one");
    limited_buf.consume(20);
    ax_assert_eq!(limited_buf.fill_buf().unwrap(), b"");

    let mut chained = (&b"aa,"[..]).chain(&b"bb,cc"[..]);
    let (left, right) = chained.get_ref();
    ax_assert_eq!(*left, b"aa,");
    ax_assert_eq!(*right, b"bb,cc");
    let mut field = Vec::new();
    ax_assert_eq!(chained.read_until(b',', &mut field).unwrap(), 3);
    ax_assert_eq!(field, b"aa,");
    field.clear();
    ax_assert_eq!(chained.read_until(b',', &mut field).unwrap(), 3);
    ax_assert_eq!(field, b"bb,");
    let (_, second) = chained.get_mut();
    ax_assert_eq!(*second, b"cc");
    let (_, second) = chained.into_inner();
    ax_assert_eq!(second, b"cc");

    #[derive(Debug)]
    struct FailingWriter {
        accepted: Vec<u8>,
        fail_after: usize,
    }

    impl Write for FailingWriter {
        fn write(&mut self, buf: &[u8]) -> ax_io::Result<usize> {
            if self.accepted.len() >= self.fail_after {
                return Err(Error::StorageFull);
            }
            let writable = (self.fail_after - self.accepted.len()).min(buf.len());
            self.accepted.extend_from_slice(&buf[..writable]);
            Ok(writable)
        }

        fn flush(&mut self) -> ax_io::Result<()> {
            Ok(())
        }
    }

    let mut writer = BufWriter::with_capacity(
        8,
        FailingWriter {
            accepted: Vec::new(),
            fail_after: 2,
        },
    );
    writer.write_all(b"abcd").unwrap();
    let into_inner_error = writer.into_inner().unwrap_err();
    ax_assert_eq!(*into_inner_error.error(), Error::StorageFull);
    ax_assert_eq!(
        format!("{into_inner_error}"),
        format!("{}", Error::StorageFull)
    );

    let (error, recovered) = into_inner_error.into_parts();
    ax_assert_eq!(error, Error::StorageFull);
    ax_assert_eq!(recovered.get_ref().accepted, b"ab");
    ax_assert_eq!(recovered.buffer(), b"cd");
}
