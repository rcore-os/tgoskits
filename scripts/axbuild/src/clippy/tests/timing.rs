use std::time::Duration;

use crate::clippy::timing::format_elapsed;

#[test]
fn elapsed_format_uses_largest_needed_units() {
    assert_eq!(format_elapsed(Duration::from_millis(250)), "250ms");
    assert_eq!(format_elapsed(Duration::from_secs(42)), "42s");
    assert_eq!(format_elapsed(Duration::from_secs(125)), "2m 5s");
    assert_eq!(format_elapsed(Duration::from_secs(3661)), "1h 1m 1s");
}
