//! Small ArceOS framebuffer facade contract, not a drawing demonstration.
use ax_std::os::arceos::api::display as api;

pub fn run() -> crate::TestResult {
    let info = api::ax_framebuffer_info().expect("display output is required");
    assert!(info.width > 1 && info.height > 0);
    let last = (info.height as usize - 1)
        .checked_mul(info.stride)
        .and_then(|row| {
            (info.width as usize - 1)
                .checked_mul(4)
                .and_then(|column| row.checked_add(column))
        })
        .expect("framebuffer offset overflow");
    let offsets = [0, last];
    let colors = [[0x21, 0x62, 0xe3, 0], [0xa1, 0x32, 0x73, 0]];
    let mut previous = None;
    // SAFETY: this test exclusively accesses the boot framebuffer; flushes
    // complete before the next CPU access, with no userspace mapping/writer.
    unsafe {
        api::ax_with_framebuffer(&mut |bytes, _| {
            let mut saved = [[0u8; 4]; 2];
            for (index, offset) in offsets.into_iter().enumerate() {
                let pixel = bytes
                    .get_mut(offset..offset.checked_add(4).unwrap())
                    .expect("invalid framebuffer stride/length");
                saved[index].copy_from_slice(pixel);
                pixel.copy_from_slice(&colors[index]);
            }
            previous = Some(saved);
        })
    }
    .expect("framebuffer write failed");
    let previous = previous.expect("framebuffer callback was not called");
    api::ax_framebuffer_flush().expect("framebuffer flush failed");
    let mut observed = None;
    // SAFETY: the same exclusive mapping remains live after flush completion.
    unsafe {
        api::ax_with_framebuffer(&mut |bytes, _| {
            let mut pixels = [[0u8; 4]; 2];
            for (index, offset) in offsets.into_iter().enumerate() {
                pixels[index].copy_from_slice(&bytes[offset..offset + 4]);
                bytes[offset..offset + 4].copy_from_slice(&previous[index]);
            }
            observed = Some(pixels);
        })
    }
    .expect("framebuffer readback failed");
    assert_eq!(
        observed,
        Some(colors),
        "framebuffer API discarded or corrupted CPU writes"
    );
    api::ax_framebuffer_flush().expect("framebuffer restoration failed");
    Ok(())
}
