use ramdisk::RamDisk;
use rdif_block::{Buffer, Interface, Request, RequestKind, RequestStatus};

fn main() {
    let mut block = RamDisk::new(16, 1024);
    let mut queue = block.create_queue().expect("queue must be created");

    let mut read = vec![0; queue.block_size() * 2];
    submit_read(&mut *queue, 3, &mut read);
    println!("read: {:?}", read);

    let size = queue.block_size();
    let mut data = vec![0xAAu8; size];
    data.extend(vec![0xBBu8; size]);

    submit_write(&mut *queue, 3, &mut data);

    let mut after = vec![0; queue.block_size() * 2];
    submit_read(&mut *queue, 3, &mut after);
    println!("after write: {:?}", after);
}

fn submit_read(queue: &mut dyn rdif_block::IQueue, start_block: usize, data: &mut [u8]) {
    let block_size = queue.block_size();
    for (offset, block) in data.chunks_exact_mut(block_size).enumerate() {
        let id = queue
            .submit_request(Request {
                block_id: start_block + offset,
                kind: RequestKind::Read(unsafe {
                    Buffer::from_raw_parts(
                        block.as_mut_ptr(),
                        block.as_mut_ptr() as u64,
                        block.len(),
                    )
                }),
            })
            .expect("read submit should succeed");
        poll_complete(queue, id);
    }
}

fn submit_write(queue: &mut dyn rdif_block::IQueue, start_block: usize, data: &mut [u8]) {
    let block_size = queue.block_size();
    for (offset, block) in data.chunks_exact_mut(block_size).enumerate() {
        let id = queue
            .submit_request(Request {
                block_id: start_block + offset,
                kind: RequestKind::Write(unsafe {
                    Buffer::from_raw_parts(
                        block.as_mut_ptr(),
                        block.as_mut_ptr() as u64,
                        block.len(),
                    )
                }),
            })
            .expect("write submit should succeed");
        poll_complete(queue, id);
    }
}

fn poll_complete(queue: &mut dyn rdif_block::IQueue, id: rdif_block::RequestId) {
    while queue.poll_request(id).expect("poll should succeed") == RequestStatus::Pending {
        std::hint::spin_loop();
    }
}
