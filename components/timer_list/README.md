# timer_list

[![Crates.io](https://img.shields.io/crates/v/timer_list)](https://crates.io/crates/timer_list)
[![Docs.rs](https://docs.rs/timer_list/badge.svg)](https://docs.rs/timer_list)
[![CI](https://github.com/arceos-org/timer_list/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/arceos-org/timer_list/actions/workflows/ci.yml)

A list of timed events that will be triggered sequentially when the timer
expires.

## Examples

```rust
use timer_list::{TimerEvent, TimerEventFn, TimerList};
use std::time::{Duration, Instant};

let mut timer_list = TimerList::new();

// set a timer that will be triggered after 1 second
let start_time = Instant::now();
timer_list.set(Duration::from_secs(1), TimerEventFn::new(|now| {
    println!("timer event after {:?}", now);
}));

while !timer_list.is_empty() {
    // check if there is any event that is expired
    let now = Instant::now().duration_since(start_time);
    if let Some((deadline, event)) = timer_list.expire_one(now) {
        // trigger the event, will print "timer event after 1.00s"
        event.callback(now);
        break;
    }
    std::thread::sleep(Duration::from_millis(10)); // relax the CPU
}
```
