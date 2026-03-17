use axio::{prelude::*, read_fn, write_fn};

#[test]
fn test_slice_read_fn() {
    let data: [u8; _] = [1, 2, 3, 4, 5];
    let mut reader = read_fn({
        let mut buf = &data[..];
        move |b| buf.read(b)
    });

    let mut out = [0; 3];
    reader.read(&mut out).unwrap();
    assert_eq!(&out, &[1, 2, 3]);
    reader.read(&mut out).unwrap();
    assert_eq!(&out[..2], &[4, 5]);
}

#[test]
fn test_slice_write_fn() {
    let mut data: [u8; 5] = [0; 5];
    let mut writer = write_fn({
        let mut buf = &mut data[..];
        move |b| buf.write(b)
    });

    writer.write(&[1, 2, 3]).unwrap();
    writer.write(&[4, 5]).unwrap();

    assert_eq!(data, [1, 2, 3, 4, 5]);
}

#[test]
fn test_copy_iofn() {
    let data: [u8; _] = [1, 2, 3, 4, 5];
    let mut reader = read_fn({
        let mut buf = &data[..];
        move |b| buf.read(b)
    });

    let mut out: [u8; 5] = [0; 5];
    let mut writer = write_fn({
        let mut buf = &mut out[..];
        move |b| buf.write(b)
    });

    axio::copy(&mut reader, &mut writer).unwrap();

    assert_eq!(out, data);
}
