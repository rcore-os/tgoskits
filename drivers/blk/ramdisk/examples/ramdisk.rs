use ramdisk::RamDisk;
use rdif_block::{Interface, Request, RequestFlags, RequestOp, RequestStatus, Segment};

fn main() {
    let mut block = RamDisk::new(16, 1024);
    let mut queue = block.create_queue().expect("queue must be created");

    let mut read = vec![0; queue.info().device.logical_block_size * 2];
    submit(&mut *queue, RequestOp::Read, 3, &mut read);
    println!("read: {:?}", read);

    let size = queue.info().device.logical_block_size;
    let mut data = vec![0xAAu8; size];
    data.extend(vec![0xBBu8; size]);

    submit(&mut *queue, RequestOp::Write, 3, &mut data);

    let mut after = vec![0; queue.info().device.logical_block_size * 2];
    submit(&mut *queue, RequestOp::Read, 3, &mut after);
    println!("after write: {:?}", after);
}

fn submit(queue: &mut dyn rdif_block::IQueue, op: RequestOp, lba: u64, data: &mut [u8]) {
    let block_size = queue.info().device.logical_block_size;
    let segment =
        unsafe { Segment::from_raw_parts(data.as_mut_ptr(), data.as_mut_ptr() as u64, data.len()) };
    let mut segments = [segment];
    let id = queue
        .submit_request(Request {
            op,
            lba,
            block_count: (data.len() / block_size) as u32,
            segments: &mut segments,
            flags: RequestFlags::NONE,
        })
        .expect("submit should succeed");
    while queue.poll_request(id).expect("poll should succeed") == RequestStatus::Pending {
        std::hint::spin_loop();
    }
}
