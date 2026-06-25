use std::time::Duration;

use chrono::Local;

pub(super) fn print_clippy_timing(elapsed: Duration) {
    let finished_at = Local::now();
    println!(
        "clippy finished at: {}",
        finished_at.format("%Y-%m-%d %H:%M:%S %z")
    );
    println!("clippy elapsed: {}", format_elapsed(elapsed));
}

pub(super) fn format_elapsed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    let millis = elapsed.subsec_millis();
    if secs == 0 {
        return format!("{}ms", millis);
    }

    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;
    if hours > 0 {
        format!("{}h {}m {}s", hours, minutes, seconds)
    } else if minutes > 0 {
        format!("{}m {}s", minutes, seconds)
    } else {
        format!("{}s", seconds)
    }
}
