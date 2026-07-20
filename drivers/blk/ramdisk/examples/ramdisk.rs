use ramdisk::RamDisk;
use rdif_block::{Interface, OwnedRequest, RequestFlags, RequestId, RequestOp, SubmitOutcome};

fn main() {
    let mut block = RamDisk::new(16, 1024);
    let mut queue = block.create_queue().expect("queue must be created");
    let request_id = RequestId::INLINE;

    let outcome = queue
        .submit_owned(
            request_id,
            OwnedRequest {
                op: RequestOp::Flush,
                lba: 0,
                block_count: 0,
                data: None,
                flags: RequestFlags::NONE,
            },
        )
        .expect("flush should succeed");

    match outcome {
        SubmitOutcome::Completed(completion) => {
            println!("request {:?}: {:?}", completion.id, completion.result);
        }
        SubmitOutcome::Queued => unreachable!("ramdisk requests complete inline"),
    }
    queue.close().expect("shutdown should succeed");
}
