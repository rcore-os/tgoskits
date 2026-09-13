use std::{
    cell::RefCell,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

#[derive(Debug, PartialEq)]
struct Values {
    boolean: bool,
    byte: u8,
    half: u16,
    word: u32,
    double: u64,
    text: [u8; 13],
}

impl Values {
    const fn initial() -> Self {
        Self {
            boolean: true,
            byte: 0xAA,
            half: 0xcafe,
            word: 0xdeadbeed,
            double: 0xa2ce05_a2ce05,
            text: *b"Hello, world!",
        }
    }
}

struct DropProbe(Arc<AtomicUsize>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Release);
    }
}

thread_local! {
    static VALUES: RefCell<Values> = const { RefCell::new(Values::initial()) };
    static DROP_PROBE: RefCell<Option<DropProbe>> = const { RefCell::new(None) };
}

pub fn run() -> crate::TestResult {
    VALUES.with_borrow(|values| assert_eq!(*values, Values::initial()));
    let drops = Arc::new(AtomicUsize::new(0));
    let mut workers = Vec::new();
    for i in 1..=10 {
        let drops = Arc::clone(&drops);
        workers.push(thread::spawn(move || {
            VALUES.with_borrow(|values| assert_eq!(*values, Values::initial()));
            DROP_PROBE.with_borrow_mut(|slot| *slot = Some(DropProbe(drops)));
            VALUES.with_borrow_mut(|values| {
                values.boolean = i % 2 == 0;
                values.byte += i as u8;
                values.half += i as u16;
                values.word += i as u32;
                values.double += i as u64;
                values.text[5] = 48 + i as u8;
            });
            thread::yield_now();
            VALUES.with_borrow(|values| {
                assert_eq!(values.boolean, i % 2 == 0);
                assert_eq!(values.byte, 0xAA + i as u8);
                assert_eq!(values.half, 0xcafe + i as u16);
                assert_eq!(values.word, 0xdeadbeed + i as u32);
                assert_eq!(values.double, 0xa2ce05_a2ce05 + i as u64);
                let mut expected_text = *b"Hello, world!";
                expected_text[5] = 48 + i as u8;
                assert_eq!(values.text, expected_text);
            });
        }));
    }
    for worker in workers {
        worker.join().expect("TLS worker panicked");
    }
    VALUES.with_borrow(|values| assert_eq!(*values, Values::initial()));
    assert_eq!(
        drops.load(Ordering::Acquire),
        10,
        "pthread exit must run std TLS destructors"
    );
    Ok(())
}
