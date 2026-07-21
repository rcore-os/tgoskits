use ramdisk::RamDisk;
use rdif_block::{OwnedRequest, RequestFlags, RequestOp};

fn main() {
    let mut block = RamDisk::new(16, 1024)
        .into_inline_device()
        .expect("ramdisk metadata must be valid");
    let completion = block.execute_owned(OwnedRequest {
        op: RequestOp::Flush,
        lba: 0,
        block_count: 0,
        data: None,
        flags: RequestFlags::NONE,
    });
    println!("request {:?}: {:?}", completion.id, completion.result);
}
