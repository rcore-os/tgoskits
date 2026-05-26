use ramdisk::RamDisk;
use rdif_block::{Buffer, Interface, RequestRead, RequestStatus, RequestWrite};

fn main() {
    let mut block = RamDisk::new(16, 1024);
    let mut read_queue = block
        .create_read_queue()
        .expect("read queue must be created");
    let mut write_queue = block
        .create_write_queue()
        .expect("write queue must be created");

    let mut read = vec![0; read_queue.block_size() * 2];
    submit_read(&mut *read_queue, 3, &mut read);
    println!("read: {:?}", read);

    let size = write_queue.block_size();
    let mut data = vec![0xAAu8; size];
    data.extend(vec![0xBBu8; size]);

    submit_write(&mut *write_queue, 3, &mut data);

    let mut after = vec![0; read_queue.block_size() * 2];
    submit_read(&mut *read_queue, 3, &mut after);
    println!("after write: {:?}", after);
}

fn submit_read(queue: &mut dyn rdif_block::IReadQueue, start_block: usize, data: &mut [u8]) {
    let block_size = queue.block_size();
    for (offset, block) in data.chunks_exact_mut(block_size).enumerate() {
        let id = queue
            .submit_read(RequestRead {
                block_id: start_block + offset,
                buffer: unsafe {
                    Buffer::from_raw_parts(
                        block.as_mut_ptr(),
                        block.as_mut_ptr() as u64,
                        block.len(),
                    )
                },
            })
            .expect("read submit should succeed");
        poll_complete(queue, id);
    }
}

fn submit_write(queue: &mut dyn rdif_block::IWriteQueue, start_block: usize, data: &mut [u8]) {
    let block_size = queue.block_size();
    for (offset, block) in data.chunks_exact_mut(block_size).enumerate() {
        let id = queue
            .submit_write(RequestWrite {
                block_id: start_block + offset,
                buffer: unsafe {
                    Buffer::from_raw_parts(
                        block.as_mut_ptr(),
                        block.as_mut_ptr() as u64,
                        block.len(),
                    )
                },
            })
            .expect("write submit should succeed");
        poll_write_complete(queue, id);
    }
}

fn poll_complete(queue: &mut dyn rdif_block::IReadQueue, id: rdif_block::RequestId) {
    while queue.poll_read(id).expect("poll should succeed") == RequestStatus::Pending {
        std::hint::spin_loop();
    }
}

fn poll_write_complete(queue: &mut dyn rdif_block::IWriteQueue, id: rdif_block::RequestId) {
    while queue.poll_write(id).expect("poll should succeed") == RequestStatus::Pending {
        std::hint::spin_loop();
    }
}
