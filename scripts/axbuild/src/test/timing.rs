use std::time::{Duration, Instant};

const TIMING_PREFIX: &str = "AXBUILD_TIMING:";

#[derive(Debug)]
pub(crate) struct TimingStage {
    scope: &'static str,
    fields: Vec<(&'static str, String)>,
    started: Instant,
}

impl TimingStage {
    pub(crate) fn new(
        scope: &'static str,
        fields: impl IntoIterator<Item = (&'static str, String)>,
    ) -> Self {
        Self {
            scope,
            fields: fields.into_iter().collect(),
            started: Instant::now(),
        }
    }

    pub(crate) fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub(crate) fn finish(self) -> Duration {
        let elapsed = self.started.elapsed();
        print_timing_line(self.scope, &self.fields, elapsed);
        elapsed
    }
}

pub(crate) fn print_timing_line(scope: &str, fields: &[(&'static str, String)], elapsed: Duration) {
    println!("{}", format_timing_line(scope, fields, elapsed));
}

pub(crate) fn print_grouped_c_compile_total(case: &str, mode: &str, elapsed: Duration) {
    println!(
        "{}",
        format_grouped_c_compile_total_line(case, mode, elapsed)
    );
}

pub(crate) fn format_grouped_c_compile_total_line(
    case: &str,
    mode: &str,
    elapsed: Duration,
) -> String {
    format_timing_line(
        "grouped-c",
        &[("case", case), ("phase", "compile-total"), ("mode", mode)],
        elapsed,
    )
}

pub(crate) fn format_timing_line(
    scope: &str,
    fields: &[(&'static str, impl AsRef<str>)],
    elapsed: Duration,
) -> String {
    let mut line = format!("{TIMING_PREFIX} scope={}", normalize_timing_value(scope));
    for (key, value) in fields {
        line.push(' ');
        line.push_str(key);
        line.push('=');
        line.push_str(&normalize_timing_value(value.as_ref()));
    }
    line.push_str(&format!(" elapsed_s={:.3}", elapsed.as_secs_f64()));
    line
}

fn normalize_timing_value(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_ascii_whitespace() { '-' } else { ch })
        .collect()
}
