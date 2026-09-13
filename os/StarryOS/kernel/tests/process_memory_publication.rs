#[path = "../src/task/process_memory/reader_epoch.rs"]
mod reader_epoch;

#[test]
fn reader_admission_prevents_reclaiming_its_loaded_generation() {
    loom::model(|| {
        use loom::{
            sync::{
                Arc,
                atomic::{AtomicBool, AtomicUsize, Ordering},
            },
            thread,
        };
        let epochs = Arc::new(reader_epoch::ReaderEpoch::new());
        let pointer = Arc::new(AtomicUsize::new(0));
        let reclaimed = Arc::new([AtomicBool::new(false), AtomicBool::new(false)]);
        let reader = {
            let epochs = epochs.clone();
            let pointer = pointer.clone();
            let reclaimed = reclaimed.clone();
            thread::spawn(move || {
                if let Some(epoch) = epochs.enter() {
                    let generation = pointer.load(Ordering::Acquire);
                    // This interval models the raw-pointer load through the
                    // acquisition of an independent Arc strong reference.
                    if generation < reclaimed.len() {
                        assert!(
                            !reclaimed[generation].load(Ordering::Acquire),
                            "the old publication was freed while a raw reader still needed it"
                        );
                    }
                    epochs.leave(epoch);
                }
            })
        };
        let writer = {
            let epochs = epochs.clone();
            let pointer = pointer.clone();
            let reclaimed = reclaimed.clone();
            thread::spawn(move || {
                // Reuse the two-slot epoch once, including a reader whose
                // initial epoch load predates both replacements.
                for generation in 0..reclaimed.len() {
                    pointer.swap(generation + 1, Ordering::AcqRel);
                    let previous_epoch = epochs.advance();
                    if !epochs.is_quiescent(previous_epoch) {
                        // Production waits here. End this explored writer
                        // prefix rather than start a second closure early.
                        return;
                    }
                    reclaimed[generation].store(true, Ordering::Release);
                }
            })
        };
        reader.join().unwrap();
        writer.join().unwrap();
    });
}
