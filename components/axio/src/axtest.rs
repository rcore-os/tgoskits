use alloc::{
    boxed::Box,
    collections::VecDeque,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use core::{io::BorrowedBuf, mem::MaybeUninit};

use axtest::prelude::*;

use crate as ax_io;

#[axtest]
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

#[axtest]
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

#[axtest]
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

#[axtest]
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

#[axtest]
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

#[axtest]
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

#[axtest]
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

#[axtest]
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

#[axtest]
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

#[axtest]
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

#[axtest]
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

#[axtest]
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

#[axtest]
fn axio_buffered_reader_edge_paths_hold() {
    use ax_io::{BufRead, BufReader, Cursor, Error, Read, Seek, SeekFrom};

    let mut reader = BufReader::new(&b"direct-read"[..]);
    ax_assert_eq!(reader.capacity(), ax_io::DEFAULT_BUF_SIZE);
    ax_assert_eq!(reader.get_ref(), &&b"direct-read"[..]);
    ax_assert!(!reader.initialized());
    ax_assert!(format!("{reader:?}").contains("BufReader"));

    let mut direct = [0; 11];
    reader.read_exact(&mut direct).unwrap();
    ax_assert_eq!(&direct, b"direct-read");
    ax_assert!(reader.buffer().is_empty());
    ax_assert_eq!(reader.into_inner(), &b""[..]);

    let mut reader = BufReader::with_capacity(4, &b"abcdef"[..]);
    ax_assert_eq!(reader.peek(3).unwrap(), b"abc");
    let mut fast = [0; 2];
    reader.read_exact(&mut fast).unwrap();
    ax_assert_eq!(&fast, b"ab");
    ax_assert_eq!(reader.buffer(), b"cd");

    let mut prefixed = "prefix:".to_string();
    ax_assert_eq!(reader.read_to_string(&mut prefixed).unwrap(), 4);
    ax_assert_eq!(prefixed, "prefix:cdef");

    let mut reader = BufReader::with_capacity(2, &[0xff, b'a'][..]);
    let mut text = "keep:".to_string();
    ax_assert_eq!(reader.read_to_string(&mut text), Err(Error::IllegalBytes));
    ax_assert_eq!(text, "keep:");

    let mut seekable = BufReader::with_capacity(3, Cursor::new(Vec::from(&b"012345"[..])));
    ax_assert_eq!(seekable.fill_buf().unwrap(), b"012");
    ax_assert_eq!(seekable.stream_position().unwrap(), 0);
    ax_assert_eq!(seekable.seek(SeekFrom::Current(2)).unwrap(), 2);
    ax_assert!(seekable.buffer().is_empty());
    let mut tail = String::new();
    seekable.read_to_string(&mut tail).unwrap();
    ax_assert_eq!(tail, "2345");

    *seekable.get_mut().get_mut() = Vec::from(&b"xy"[..]);
    seekable.seek(SeekFrom::Start(0)).unwrap();
    let mut reset = String::new();
    seekable.read_to_string(&mut reset).unwrap();
    ax_assert_eq!(reset, "xy");
}

#[axtest]
fn axio_buffered_writer_edge_paths_hold() {
    use ax_io::{BufWriter, Error, Write};

    let mut writer = BufWriter::new(Vec::<u8>::new());
    ax_assert_eq!(writer.capacity(), ax_io::DEFAULT_BUF_SIZE);
    ax_assert!(format!("{writer:?}").contains("BufWriter"));
    writer.write_all(b"buf").unwrap();
    ax_assert_eq!(writer.get_ref(), b"");
    writer.get_mut().extend_from_slice(b"inner-");
    ax_assert_eq!(writer.buffer(), b"buf");
    writer.flush().unwrap();
    ax_assert_eq!(writer.into_inner().unwrap(), b"inner-buf");

    let mut writer = BufWriter::with_capacity(3, Vec::<u8>::new());
    ax_assert_eq!(writer.write(b"ab").unwrap(), 2);
    ax_assert_eq!(writer.write(b"cdef").unwrap(), 4);
    ax_assert_eq!(writer.get_ref(), b"abcdef");
    ax_assert!(writer.buffer().is_empty());
    writer.write_all(b"xy").unwrap();
    let (inner, buffered) = writer.into_parts();
    ax_assert_eq!(inner, b"abcdef");
    ax_assert_eq!(buffered.unwrap(), b"xy");

    #[derive(Debug)]
    struct ZeroWriter;

    impl Write for ZeroWriter {
        fn write(&mut self, _buf: &[u8]) -> ax_io::Result<usize> {
            Ok(0)
        }

        fn flush(&mut self) -> ax_io::Result<()> {
            Ok(())
        }
    }

    let mut writer = BufWriter::with_capacity(4, ZeroWriter);
    writer.write_all(b"zz").unwrap();
    ax_assert_eq!(writer.flush(), Err(Error::WriteZero));
    let into_inner_error = writer.into_inner().unwrap_err();
    ax_assert_eq!(*into_inner_error.error(), Error::WriteZero);
}

#[axtest]
fn axio_cursor_additional_buffer_forms_hold() {
    use ax_io::{Cursor, Error, IoBuf, IoBufMut, Read, Write};

    let mut cursor = Cursor::new(Vec::from(&b"abcdef"[..]));
    cursor.set_position(3);
    {
        let (left, right) = cursor.split_mut();
        left[0] = b'A';
        right[0] = b'D';
    }
    ax_assert_eq!(cursor.get_ref(), b"AbcDef");

    let cloned = cursor.clone();
    ax_assert_eq!(cloned, cursor);
    let mut clone_target = Cursor::new(Vec::from(&b"x"[..]));
    clone_target.clone_from(&cursor);
    ax_assert_eq!(clone_target, cursor);

    cursor.set_position(4);
    let mut too_large = [0; 4];
    ax_assert_eq!(cursor.read_exact(&mut too_large), Err(Error::UnexpectedEof));
    ax_assert_eq!(cursor.position(), 6);

    let mut bad_utf8 = Cursor::new(Vec::from(&[b'o', 0xff][..]));
    let mut text = "prefix:".to_string();
    ax_assert_eq!(bad_utf8.read_to_string(&mut text), Err(Error::IllegalBytes));
    ax_assert_eq!(bad_utf8.position(), 0);
    ax_assert_eq!(text, "prefix:");

    let mut backing = Vec::from(&b"abc"[..]);
    {
        let mut cursor = Cursor::new(&mut backing);
        cursor.set_position(5);
        cursor.write_all(b"z").unwrap();
        ax_assert!(cursor.remaining_mut() > 0);
    }
    ax_assert_eq!(backing, b"abc\0\0z");

    let mut boxed = Cursor::new(Vec::from(&b"1234"[..]).into_boxed_slice());
    ax_assert_eq!(boxed.write(b"xy").unwrap(), 2);
    ax_assert_eq!(boxed.write_all(b"zzz"), Err(Error::WriteZero));
    ax_assert_eq!(boxed.into_inner().as_ref(), b"xyzz");

    let mut array = Cursor::new([0; 4]);
    array.write_all(b"hi").unwrap();
    ax_assert_eq!(array.position(), 2);
    ax_assert_eq!(array.into_inner(), *b"hi\0\0");

    let mut source = Cursor::new(&b"remain"[..]);
    source.set_position(3);
    ax_assert_eq!(source.remaining(), 3);
}

#[axtest]
fn axio_copy_error_and_adapter_edges_hold() {
    use ax_io::{Error, Read, Write, copy, read_fn, stack_buffer_copy, write_fn};

    let mut interrupted_once = true;
    let mut reader = read_fn(|buf: &mut [u8]| {
        if interrupted_once {
            interrupted_once = false;
            return Err(Error::Interrupted);
        }
        buf[..2].copy_from_slice(b"ok");
        Ok(2)
    });
    let mut out = [0; 2];
    ax_assert_eq!(reader.read(&mut out), Err(Error::Interrupted));
    ax_assert_eq!(reader.read(&mut out).unwrap(), 2);
    ax_assert_eq!(&out, b"ok");

    let mut fail_reader = read_fn(|_buf: &mut [u8]| Err(Error::InvalidData));
    let mut sink = Vec::new();
    ax_assert_eq!(
        stack_buffer_copy(&mut fail_reader, &mut sink),
        Err(Error::InvalidData)
    );

    #[derive(Debug)]
    struct ShortWriter {
        accepted: Vec<u8>,
    }

    impl Write for ShortWriter {
        fn write(&mut self, buf: &[u8]) -> ax_io::Result<usize> {
            let n = buf.len().min(1);
            self.accepted.extend_from_slice(&buf[..n]);
            Ok(n)
        }

        fn flush(&mut self) -> ax_io::Result<()> {
            Ok(())
        }
    }

    let mut source: &[u8] = b"abc";
    let mut writer = ShortWriter {
        accepted: Vec::new(),
    };
    ax_assert_eq!(copy(&mut source, &mut writer).unwrap(), 3);
    ax_assert_eq!(writer.accepted, b"abc");

    let mut fail_writer = write_fn(|_buf: &[u8]| Err(Error::StorageFull));
    ax_assert_eq!(fail_writer.write_all(b"x"), Err(Error::StorageFull));
    ax_assert_eq!(
        fail_writer.write_fmt(format_args!("x")),
        Err(Error::StorageFull)
    );

    let mut boxed: Box<dyn Write> = Box::new(Vec::<u8>::new());
    ax_assert_eq!(boxed.write_fmt(format_args!("{}{}", "bo", "x")), Ok(()));
    ax_assert_eq!(boxed.flush(), Ok(()));
}

#[axtest]
fn axio_line_writer_edge_paths_hold() {
    use ax_io::{Error, IoBufMut, LineWriter, Write};

    let mut writer = LineWriter::new(Vec::<u8>::new());
    ax_assert!(format!("{writer:?}").contains("LineWriter"));
    ax_assert!(writer.remaining_mut() > 0);
    writer.write_all(b"prefix").unwrap();
    ax_assert!(writer.get_ref().is_empty());
    writer.get_mut().extend_from_slice(b"inner-");
    writer.write_all(b"-line\n").unwrap();
    ax_assert_eq!(writer.get_ref(), b"inner-prefix-line\n");
    writer.write_fmt(format_args!("tail {}", 7)).unwrap();
    ax_assert_eq!(writer.get_ref(), b"inner-prefix-line\n");
    writer.flush().unwrap();
    ax_assert_eq!(writer.into_inner().unwrap(), b"inner-prefix-line\ntail 7");

    #[derive(Debug)]
    struct PartialWriter {
        accepted: Vec<u8>,
        max_once: usize,
    }

    impl Write for PartialWriter {
        fn write(&mut self, buf: &[u8]) -> ax_io::Result<usize> {
            let n = buf.len().min(self.max_once);
            self.accepted.extend_from_slice(&buf[..n]);
            Ok(n)
        }

        fn flush(&mut self) -> ax_io::Result<()> {
            Ok(())
        }
    }

    let mut writer = LineWriter::with_capacity(
        4,
        PartialWriter {
            accepted: Vec::new(),
            max_once: 2,
        },
    );
    let input = b"ab\ncd";
    let written = writer.write(input).unwrap();
    ax_assert_eq!(written, 3);
    ax_assert_eq!(writer.get_ref().accepted, b"ab");
    writer.write_all(&input[written..]).unwrap();
    writer.write_all(b"ef\n").unwrap();
    ax_assert_eq!(writer.get_ref().accepted, b"ab\ncdef\n");

    #[derive(Debug)]
    struct FailingLineWriter {
        accepted: Vec<u8>,
        fail_after: usize,
    }

    impl Write for FailingLineWriter {
        fn write(&mut self, buf: &[u8]) -> ax_io::Result<usize> {
            if self.accepted.len() >= self.fail_after {
                return Err(Error::StorageFull);
            }
            let n = (self.fail_after - self.accepted.len()).min(buf.len());
            self.accepted.extend_from_slice(&buf[..n]);
            Ok(n)
        }

        fn flush(&mut self) -> ax_io::Result<()> {
            Ok(())
        }
    }

    let mut writer = LineWriter::with_capacity(
        8,
        FailingLineWriter {
            accepted: Vec::new(),
            fail_after: 2,
        },
    );
    writer.write_all(b"abcd").unwrap();
    let error = writer.into_inner().unwrap_err();
    ax_assert_eq!(*error.error(), Error::StorageFull);
    let (error, recovered) = error.into_parts();
    ax_assert_eq!(error, Error::StorageFull);
    ax_assert_eq!(recovered.get_ref().accepted, b"ab");
}

#[axtest]
fn axio_seek_forwarding_and_default_rules_hold() {
    use ax_io::{Cursor, Error, Seek, SeekFrom, default_stream_len};

    let mut cursor = Cursor::new(Vec::from(&b"seekable"[..]));
    {
        let by_ref: &mut dyn Seek = &mut cursor;
        ax_assert_eq!(by_ref.seek(SeekFrom::Start(2)).unwrap(), 2);
        by_ref.seek_relative(3).unwrap();
        ax_assert_eq!(by_ref.stream_position().unwrap(), 5);
        by_ref.rewind().unwrap();
    }
    ax_assert_eq!(cursor.position(), 0);

    let mut boxed: Box<dyn Seek> = Box::new(Cursor::new(Vec::from(&b"boxed"[..])));
    ax_assert_eq!(boxed.seek(SeekFrom::End(-1)).unwrap(), 4);
    ax_assert_eq!(boxed.stream_position().unwrap(), 4);
    boxed.rewind().unwrap();
    ax_assert_eq!(boxed.stream_position().unwrap(), 0);

    #[derive(Debug)]
    struct DefaultSeek {
        pos: u64,
        len: u64,
        seeks: usize,
    }

    impl Seek for DefaultSeek {
        fn seek(&mut self, pos: SeekFrom) -> ax_io::Result<u64> {
            self.seeks += 1;
            let next = match pos {
                SeekFrom::Start(offset) => Some(offset),
                SeekFrom::End(offset) => self.len.checked_add_signed(offset),
                SeekFrom::Current(offset) => self.pos.checked_add_signed(offset),
            }
            .ok_or(Error::InvalidInput)?;
            self.pos = next;
            Ok(next)
        }
    }

    let mut seekable = DefaultSeek {
        pos: 2,
        len: 8,
        seeks: 0,
    };
    ax_assert_eq!(default_stream_len(&mut seekable).unwrap(), 8);
    ax_assert_eq!(seekable.pos, 2);
    ax_assert_eq!(seekable.seeks, 3);

    seekable.pos = 8;
    seekable.seeks = 0;
    ax_assert_eq!(seekable.stream_len().unwrap(), 8);
    ax_assert_eq!(seekable.seeks, 2);
}

#[axtest]
fn axio_chain_take_and_iobuf_edges_hold() {
    use ax_io::{BufRead, Cursor, Error, IoBuf, IoBufMut, Read, Seek, SeekFrom, Write};

    let mut chained = (&b""[..]).chain(&b"second\npart"[..]);
    ax_assert_eq!(chained.remaining(), 11);
    ax_assert_eq!(chained.fill_buf().unwrap(), b"second\npart");
    let mut field = Vec::new();
    ax_assert_eq!(chained.read_until(b'\n', &mut field).unwrap(), 7);
    ax_assert_eq!(field, b"second\n");
    chained.consume(2);
    ax_assert_eq!(chained.fill_buf().unwrap(), b"rt");

    let mut chained = (&b"left"[..]).chain(&b"right"[..]);
    let mut empty = [0; 0];
    ax_assert_eq!(chained.read(&mut empty).unwrap(), 0);
    let mut one = [0; 1];
    ax_assert_eq!(chained.read(&mut one).unwrap(), 1);
    ax_assert_eq!(&one, b"l");

    let mut take = Cursor::new(Vec::from(&b"abcdef"[..])).take(2);
    let mut buf = [0; 4];
    ax_assert_eq!(take.read(&mut buf).unwrap(), 2);
    ax_assert_eq!(&buf[..2], b"ab");
    ax_assert_eq!(take.read(&mut buf).unwrap(), 0);
    take.set_limit(3);
    ax_assert_eq!(take.limit(), 3);
    ax_assert_eq!(take.position(), 0);
    ax_assert_eq!(take.seek(SeekFrom::End(-1)).unwrap(), 2);
    ax_assert_eq!(take.limit(), 1);
    ax_assert_eq!(take.seek(SeekFrom::Start(4)), Err(Error::InvalidInput));
    take.seek_relative(-1).unwrap();
    ax_assert_eq!(take.position(), 1);

    let mut limited = (&b"abcde"[..]).take(3);
    ax_assert_eq!(limited.fill_buf().unwrap(), b"abc");
    limited.consume(99);
    ax_assert_eq!(limited.limit(), 0);
    ax_assert_eq!(limited.fill_buf().unwrap(), b"");

    let mut fixed = [0; 2];
    let mut borrowed = &mut fixed[..];
    ax_assert_eq!(borrowed.remaining_mut(), 2);
    borrowed.write_all(b"xy").unwrap();
    ax_assert_eq!(borrowed.remaining_mut(), 0);
    ax_assert_eq!(&fixed, b"xy");

    let mut deque = VecDeque::new();
    ax_assert!(deque.remaining_mut() > 0);
    deque.write_all(b"deq").unwrap();
    ax_assert_eq!(deque.len(), 3);
}

#[axtest]
fn axio_empty_repeat_sink_edge_rules_hold() {
    use ax_io::{
        BufRead, Error, IoBuf, IoBufMut, Read, Seek, SeekFrom, Write, empty, repeat, sink,
    };

    let mut empty_reader = empty();
    let mut byte = [7_u8; 1];
    ax_assert_eq!(empty_reader.read(&mut byte).unwrap(), 0);
    ax_assert_eq!(byte, [7]);
    ax_assert_eq!(empty_reader.read_exact(&mut []), Ok(()));
    ax_assert_eq!(
        empty_reader.read_exact(&mut byte),
        Err(Error::UnexpectedEof)
    );

    let mut raw = [MaybeUninit::<u8>::uninit(); 3];
    let mut borrowed: BorrowedBuf<'_, u8> = raw.as_mut_slice().into();
    empty_reader.read_buf(borrowed.unfilled()).unwrap();
    ax_assert_eq!(borrowed.filled(), b"");

    let mut raw = [MaybeUninit::<u8>::uninit(); 2];
    let mut borrowed: BorrowedBuf<'_, u8> = raw.as_mut_slice().into();
    ax_assert_eq!(
        empty_reader.read_buf_exact(borrowed.unfilled()),
        Err(Error::UnexpectedEof)
    );
    ax_assert_eq!(borrowed.filled(), b"");

    ax_assert_eq!(empty_reader.fill_buf().unwrap(), b"");
    ax_assert_eq!(empty_reader.skip_until(b'\n').unwrap(), 0);
    ax_assert!(!empty_reader.has_data_left().unwrap());
    empty_reader.consume(usize::MAX);

    let mut bytes = Vec::from(&b"prefix"[..]);
    ax_assert_eq!(empty_reader.read_to_end(&mut bytes).unwrap(), 0);
    ax_assert_eq!(bytes, b"prefix");
    let mut text = "prefix".to_string();
    ax_assert_eq!(empty_reader.read_to_string(&mut text).unwrap(), 0);
    ax_assert_eq!(text, "prefix");
    let mut until = Vec::from(&b"kept"[..]);
    ax_assert_eq!(empty_reader.read_until(b'x', &mut until).unwrap(), 0);
    ax_assert_eq!(until, b"kept");
    let mut line = "line".to_string();
    ax_assert_eq!(empty_reader.read_line(&mut line).unwrap(), 0);
    ax_assert_eq!(line, "line");

    ax_assert_eq!(empty_reader.seek(SeekFrom::Start(99)).unwrap(), 0);
    ax_assert_eq!(empty_reader.seek(SeekFrom::End(-5)).unwrap(), 0);
    ax_assert_eq!(empty_reader.stream_len().unwrap(), 0);
    ax_assert_eq!(empty_reader.stream_position().unwrap(), 0);
    ax_assert_eq!(empty_reader.remaining(), 0);
    ax_assert_eq!(empty_reader.remaining_mut(), usize::MAX);
    ax_assert_eq!(empty_reader.write(b"discarded").unwrap(), 9);
    empty_reader.write_all(b"all-discarded").unwrap();
    empty_reader
        .write_fmt(format_args!("{} {}", "fmt", 1))
        .unwrap();
    empty_reader.flush().unwrap();
    ax_assert!(format!("{empty_reader:?}").contains("Empty"));

    let empty_ref = empty();
    let mut empty_ref_writer = &empty_ref;
    ax_assert_eq!(empty_ref_writer.write(b"borrowed").unwrap(), 8);
    empty_ref_writer.write_all(b"borrowed-all").unwrap();
    empty_ref_writer
        .write_fmt(format_args!("borrowed {}", 2))
        .unwrap();
    empty_ref_writer.flush().unwrap();

    let mut repeated = repeat(b'Z');
    let mut buf = [0_u8; 5];
    ax_assert_eq!(repeated.read(&mut buf).unwrap(), 5);
    ax_assert_eq!(&buf, b"ZZZZZ");
    repeated.read_exact(&mut buf[..2]).unwrap();
    ax_assert_eq!(&buf[..2], b"ZZ");
    let mut raw = [MaybeUninit::<u8>::uninit(); 4];
    let mut borrowed: BorrowedBuf<'_, u8> = raw.as_mut_slice().into();
    repeated.read_buf(borrowed.unfilled()).unwrap();
    ax_assert_eq!(borrowed.filled(), b"ZZZZ");
    let mut raw = [MaybeUninit::<u8>::uninit(); 3];
    let mut borrowed: BorrowedBuf<'_, u8> = raw.as_mut_slice().into();
    repeated.read_buf_exact(borrowed.unfilled()).unwrap();
    ax_assert_eq!(borrowed.filled(), b"ZZZ");
    ax_assert_eq!(repeated.read_to_end(&mut Vec::new()), Err(Error::NoMemory));
    ax_assert_eq!(
        repeated.read_to_string(&mut String::new()),
        Err(Error::NoMemory)
    );
    ax_assert_eq!(repeated.remaining(), usize::MAX);
    ax_assert!(format!("{repeated:?}").contains("Repeat"));

    let mut sink_writer = sink();
    ax_assert_eq!(sink_writer.write(b"abc").unwrap(), 3);
    sink_writer.write_all(b"def").unwrap();
    sink_writer
        .write_fmt(format_args!("{}{}", "g", "h"))
        .unwrap();
    sink_writer.flush().unwrap();
    ax_assert_eq!(sink_writer.remaining_mut(), usize::MAX);
    ax_assert!(format!("{sink_writer:?}").contains("Sink"));

    let sink_ref = sink();
    let mut sink_ref_writer = &sink_ref;
    ax_assert_eq!(sink_ref_writer.write(b"abc").unwrap(), 3);
    sink_ref_writer.write_all(b"def").unwrap();
    sink_ref_writer
        .write_fmt(format_args!("{}{}", "g", "h"))
        .unwrap();
    sink_ref_writer.flush().unwrap();
}

#[axtest]
fn axio_vecdeque_split_read_and_bufread_rules_hold() {
    use ax_io::{BufRead, Error, Read};

    let mut deque = VecDeque::with_capacity(8);
    deque.extend(*b"abcdef");
    ax_assert_eq!(deque.drain(..4).collect::<Vec<_>>(), b"abcd");
    deque.extend(*b"ghijkl");
    let (front, back) = deque.as_slices();
    ax_assert!(!front.is_empty());
    ax_assert!(!back.is_empty());

    let mut first = [0_u8; 4];
    deque.read_exact(&mut first).unwrap();
    ax_assert_eq!(&first, b"efgh");
    ax_assert_eq!(deque.iter().copied().collect::<Vec<_>>(), b"ijkl");

    let mut short = [0_u8; 3];
    ax_assert_eq!(deque.read(&mut short).unwrap(), 3);
    ax_assert_eq!(&short, b"ijk");
    ax_assert_eq!(deque.iter().copied().collect::<Vec<_>>(), b"l");

    let mut too_large = [0_u8; 2];
    ax_assert_eq!(deque.read_exact(&mut too_large), Err(Error::UnexpectedEof));
    ax_assert!(deque.is_empty());

    let mut deque = VecDeque::with_capacity(6);
    deque.extend(*b"0123");
    ax_assert_eq!(deque.drain(..3).collect::<Vec<_>>(), b"012");
    deque.extend(*b"456789");
    let mut raw = [MaybeUninit::<u8>::uninit(); 5];
    let mut borrowed: BorrowedBuf<'_, u8> = raw.as_mut_slice().into();
    deque.read_buf_exact(borrowed.unfilled()).unwrap();
    ax_assert_eq!(borrowed.filled(), b"34567");
    ax_assert_eq!(deque.iter().copied().collect::<Vec<_>>(), b"89");

    let mut raw = [MaybeUninit::<u8>::uninit(); 4];
    let mut borrowed: BorrowedBuf<'_, u8> = raw.as_mut_slice().into();
    ax_assert_eq!(
        deque.read_buf_exact(borrowed.unfilled()),
        Err(Error::UnexpectedEof)
    );
    ax_assert_eq!(borrowed.filled(), b"89");
    ax_assert!(deque.is_empty());

    let mut deque = VecDeque::from(Vec::from(&b"front-back"[..]));
    ax_assert_eq!(deque.fill_buf().unwrap(), b"front-back");
    deque.consume(6);
    ax_assert_eq!(deque.fill_buf().unwrap(), b"back");
    let mut text = "prefix:".to_string();
    ax_assert_eq!(deque.read_to_string(&mut text).unwrap(), 4);
    ax_assert_eq!(text, "prefix:back");

    let mut invalid = VecDeque::from(Vec::from(&[0xff, b'a'][..]));
    let mut text = String::new();
    ax_assert_eq!(invalid.read_to_string(&mut text), Err(Error::IllegalBytes));
}

#[axtest]
fn axio_iobuf_extension_specialization_rules_hold() {
    use ax_io::{BufReader, BufWriter, IoBuf, IoBufExt, IoBufMut, IoBufMutExt, Read, Write};

    let mut source: &[u8] = b"slice-copy";
    let mut output = Vec::new();
    ax_assert_eq!(source.write_to(&mut output).unwrap(), 10);
    ax_assert_eq!(output, b"slice-copy");
    ax_assert_eq!(source, b"slice-copy");

    let mut fixed = [0_u8; 4];
    {
        let mut fixed_writer = &mut fixed[..];
        let mut reader: &[u8] = b"abcdef";
        ax_assert_eq!(fixed_writer.read_from(&mut reader).unwrap(), 4);
        ax_assert_eq!(reader, b"ef");
    }
    ax_assert_eq!(&fixed, b"abcd");

    let mut raw = [MaybeUninit::<u8>::uninit(); 5];
    let mut borrowed: BorrowedBuf<'_, u8> = raw.as_mut_slice().into();
    {
        let mut cursor = borrowed.unfilled();
        let mut reader: &[u8] = b"cursor-tail";
        ax_assert_eq!(cursor.read_from(&mut reader).unwrap(), 5);
        ax_assert_eq!(reader, b"r-tail");
    }
    ax_assert_eq!(borrowed.filled(), b"curso");

    let mut growable = Vec::with_capacity(8);
    let mut reader: &[u8] = b"grow";
    ax_assert_eq!(growable.read_from(&mut reader).unwrap(), 4);
    ax_assert_eq!(growable, b"grow");
    ax_assert_eq!(reader, b"");

    let mut writer = BufWriter::with_capacity(8, Vec::<u8>::new());
    ax_assert_eq!(writer.remaining_mut(), isize::MAX as usize);
    let mut reader: &[u8] = b"buffered";
    ax_assert_eq!(writer.read_from(&mut reader).unwrap(), 8);
    ax_assert_eq!(writer.buffer(), b"buffered");
    ax_assert_eq!(reader, b"");
    writer.flush().unwrap();
    ax_assert_eq!(writer.into_inner().unwrap(), b"buffered");

    let mut reader = BufReader::with_capacity(4, &b"reader-buffer"[..]);
    let mut output = Vec::new();
    ax_assert_eq!(reader.write_to(&mut output).unwrap(), 4);
    ax_assert_eq!(output, b"read");
    ax_assert_eq!(reader.write_to(&mut output).unwrap(), 4);
    ax_assert_eq!(output, b"reader-b");
    let mut rest = Vec::new();
    reader.read_to_end(&mut rest).unwrap();
    ax_assert_eq!(rest, b"uffer");
    ax_assert_eq!(reader.remaining(), 0);
}

#[axtest]
fn axio_forwarding_box_and_borrowed_writer_rules_hold() {
    use ax_io::{Error, Read, Write};

    let mut input: &[u8] = b"borrowed-read";
    {
        let mut borrowed: &mut dyn Read = &mut input;
        let through_ref = &mut borrowed;
        let mut head = [0_u8; 8];
        through_ref.read_exact(&mut head).unwrap();
        ax_assert_eq!(&head, b"borrowed");
    }
    ax_assert_eq!(input, b"-read");

    let mut boxed_slice: Box<&mut dyn Read> = Box::new(&mut input);
    let mut tail = String::new();
    boxed_slice.read_to_string(&mut tail).unwrap();
    ax_assert_eq!(tail, "-read");

    let mut output = Vec::new();
    {
        let mut borrowed: &mut dyn Write = &mut output;
        let through_ref = &mut borrowed;
        ax_assert_eq!(through_ref.write(b"borrowed").unwrap(), 8);
        through_ref.write_all(b"-write").unwrap();
        through_ref.write_fmt(format_args!("-{}", "fmt")).unwrap();
        through_ref.flush().unwrap();
    }
    ax_assert_eq!(output, b"borrowed-write-fmt");

    let mut boxed: Box<dyn Write> = Box::new(Vec::<u8>::new());
    boxed.write_all(b"boxed").unwrap();
    boxed.write_fmt(format_args!("-{}", 7)).unwrap();
    boxed.flush().unwrap();

    let mut fixed = [0_u8; 3];
    let mut writer = &mut fixed[..];
    ax_assert_eq!(writer.write(b"abcdef").unwrap(), 3);
    ax_assert_eq!(writer.write(b"x").unwrap(), 0);
    ax_assert_eq!(writer.write_all(b"x"), Err(Error::WriteZero));
    ax_assert_eq!(&fixed, b"abc");
}

#[axtest]
fn axio_borrowed_cursor_writer_rules_hold() {
    use ax_io::{Error, Write};

    let mut raw = [MaybeUninit::<u8>::uninit(); 4];
    let mut borrowed: BorrowedBuf<'_, u8> = raw.as_mut_slice().into();
    {
        let mut cursor = borrowed.unfilled();
        ax_assert_eq!(cursor.write(b"abcdef").unwrap(), 4);
        ax_assert_eq!(cursor.write(b"x").unwrap(), 0);
        ax_assert_eq!(cursor.flush(), Ok(()));
    }
    ax_assert_eq!(borrowed.filled(), b"abcd");

    let mut raw = [MaybeUninit::<u8>::uninit(); 2];
    let mut borrowed: BorrowedBuf<'_, u8> = raw.as_mut_slice().into();
    {
        let mut cursor = borrowed.unfilled();
        ax_assert_eq!(cursor.write_all(b"xy"), Ok(()));
        ax_assert_eq!(cursor.write_all(b"z"), Err(Error::WriteZero));
    }
    ax_assert_eq!(borrowed.filled(), b"xy");
}

#[axtest]
fn axio_copy_and_default_transfer_edges_hold() {
    use ax_io::{Error, IoBuf, IoBufExt, IoBufMut, IoBufMutExt, Read, Result, Write, copy};

    struct TinyRead {
        chunks: Vec<&'static [u8]>,
    }

    impl Read for TinyRead {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
            let Some(chunk) = self.chunks.first_mut() else {
                return Ok(0);
            };
            let copied = buf.len().min(chunk.len());
            buf[..copied].copy_from_slice(&chunk[..copied]);
            *chunk = &chunk[copied..];
            if chunk.is_empty() {
                self.chunks.remove(0);
            }
            Ok(copied)
        }
    }

    impl IoBuf for TinyRead {
        fn remaining(&self) -> usize {
            self.chunks.iter().map(|chunk| chunk.len()).sum()
        }
    }

    struct LimitWrite {
        limit: usize,
        output: Vec<u8>,
    }

    impl Write for LimitWrite {
        fn write(&mut self, buf: &[u8]) -> Result<usize> {
            let copied = self.limit.min(buf.len());
            self.output.extend_from_slice(&buf[..copied]);
            Ok(copied)
        }

        fn flush(&mut self) -> Result<()> {
            Ok(())
        }
    }

    impl IoBufMut for LimitWrite {
        fn remaining_mut(&self) -> usize {
            self.limit
        }
    }

    let mut reader = TinyRead {
        chunks: vec![b"ab", b"cd", b"ef"],
    };
    let mut writer = LimitWrite {
        limit: 3,
        output: Vec::new(),
    };
    ax_assert_eq!(reader.write_to(&mut writer).unwrap(), 2);
    ax_assert_eq!(writer.output, b"ab");

    let mut reader = TinyRead {
        chunks: vec![b"abcd"],
    };
    let mut writer = LimitWrite {
        limit: 2,
        output: Vec::new(),
    };
    ax_assert_eq!(writer.read_from(&mut reader).unwrap(), 2);
    ax_assert_eq!(writer.output, b"ab");

    let mut reader = TinyRead {
        chunks: vec![b"hello", b"-world"],
    };
    let mut output = Vec::new();
    ax_assert_eq!(copy(&mut reader, &mut output).unwrap(), 11);
    ax_assert_eq!(output, b"hello-world");

    struct FailingRead;

    impl Read for FailingRead {
        fn read(&mut self, _buf: &mut [u8]) -> Result<usize> {
            Err(Error::BrokenPipe)
        }
    }

    let mut failing = FailingRead;
    let mut output = Vec::new();
    ax_assert_eq!(copy(&mut failing, &mut output), Err(Error::BrokenPipe));
}

#[axtest]
fn axio_vecdeque_wrapped_buffer_read_paths_hold() {
    use ax_io::{BufRead, Error, Read};

    let mut deque = VecDeque::with_capacity(8);
    deque.extend(b"abcdef");
    let mut scratch = [0_u8; 4];
    ax_assert_eq!(deque.read(&mut scratch).unwrap(), 4);
    ax_assert_eq!(&scratch, b"abcd");
    deque.extend(b"ghijk");
    let mut joined = [0_u8; 7];
    deque.read_exact(&mut joined).unwrap();
    ax_assert_eq!(&joined, b"efghijk");
    ax_assert!(deque.is_empty());

    let mut deque = VecDeque::with_capacity(8);
    deque.extend(b"abcdef");
    let mut drain = [0_u8; 5];
    ax_assert_eq!(deque.read(&mut drain).unwrap(), 5);
    ax_assert_eq!(&drain, b"abcde");
    deque.extend(b"ghij");
    let mut too_large = [0_u8; 8];
    ax_assert_eq!(deque.read_exact(&mut too_large), Err(Error::UnexpectedEof));
    ax_assert!(deque.is_empty());

    let mut deque = VecDeque::with_capacity(8);
    deque.extend(b"abcdef");
    let mut head = [0_u8; 5];
    ax_assert_eq!(deque.read(&mut head).unwrap(), 5);
    deque.extend(b"ghij");
    let mut raw = [MaybeUninit::<u8>::uninit(); 5];
    let mut borrowed: BorrowedBuf<'_, u8> = raw.as_mut_slice().into();
    deque.read_buf_exact(borrowed.unfilled()).unwrap();
    ax_assert_eq!(borrowed.filled(), b"fghij");
    ax_assert!(deque.is_empty());

    let mut deque = VecDeque::with_capacity(8);
    deque.extend(b"abc");
    let mut raw = [MaybeUninit::<u8>::uninit(); 4];
    let mut borrowed: BorrowedBuf<'_, u8> = raw.as_mut_slice().into();
    ax_assert_eq!(
        deque.read_buf_exact(borrowed.unfilled()),
        Err(Error::UnexpectedEof)
    );
    ax_assert_eq!(borrowed.filled(), b"abc");
    ax_assert!(deque.is_empty());

    let mut deque = VecDeque::from(Vec::from(&b"line\nrest"[..]));
    ax_assert_eq!(deque.fill_buf().unwrap(), b"line\nrest");
    deque.consume(5);
    ax_assert_eq!(deque.fill_buf().unwrap(), b"rest");

    let mut deque = VecDeque::from(Vec::from("utf8".as_bytes()));
    let mut text = "prefix:".to_string();
    ax_assert_eq!(deque.read_to_string(&mut text).unwrap(), 4);
    ax_assert_eq!(text, "prefix:utf8");
}

#[axtest]
fn axio_boxed_bufread_and_seek_forwarding_rules_hold() {
    use ax_io::{BufRead, Cursor, Seek, SeekFrom};

    let deque = VecDeque::from(Vec::from(&b"alpha\nbeta"[..]));
    let mut boxed_buf: Box<dyn BufRead> = Box::new(deque);
    ax_assert!(boxed_buf.has_data_left().unwrap());
    let mut first = Vec::new();
    ax_assert_eq!(boxed_buf.read_until(b'\n', &mut first).unwrap(), 6);
    ax_assert_eq!(first, b"alpha\n");
    let mut second = String::new();
    ax_assert_eq!(boxed_buf.read_line(&mut second).unwrap(), 4);
    ax_assert_eq!(second, "beta");
    ax_assert!(!boxed_buf.has_data_left().unwrap());

    let cursor = Cursor::new(Vec::from(&b"seekable"[..]));
    let mut boxed_seek: Box<dyn Seek> = Box::new(cursor);
    ax_assert_eq!(boxed_seek.stream_position().unwrap(), 0);
    ax_assert_eq!(boxed_seek.seek(SeekFrom::Start(4)).unwrap(), 4);
    boxed_seek.seek_relative(-2).unwrap();
    ax_assert_eq!(boxed_seek.stream_position().unwrap(), 2);
    ax_assert_eq!(boxed_seek.stream_len().unwrap(), 8);
    boxed_seek.rewind().unwrap();
    ax_assert_eq!(boxed_seek.stream_position().unwrap(), 0);
}
