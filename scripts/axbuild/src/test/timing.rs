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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    #[test]
    fn axbuild_timing_line_contains_stable_fields_and_parseable_seconds() {
        let line = super::format_timing_line(
            "starry-qemu",
            &[("build_group", "qemu-smp1"), ("phase", "build")],
            Duration::from_millis(1250),
        );

        assert!(line.starts_with("AXBUILD_TIMING: "));
        assert!(line.contains("scope=starry-qemu"));
        assert!(line.contains("build_group=qemu-smp1"));
        assert!(line.contains("phase=build"));

        let elapsed = line
            .split_ascii_whitespace()
            .find_map(|field| field.strip_prefix("elapsed_s="))
            .expect("timing line must include elapsed_s");
        let elapsed = elapsed
            .parse::<f64>()
            .expect("elapsed_s must be a parseable floating point second value");
        assert!((elapsed - 1.25).abs() < f64::EPSILON);
    }

    #[test]
    fn axbuild_timing_line_normalizes_field_whitespace() {
        let line = super::format_timing_line(
            "qemu case",
            &[
                ("case", "qemu-smp1/system test"),
                ("phase", "prepare assets"),
            ],
            Duration::from_millis(1),
        );

        assert!(line.contains("scope=qemu-case"));
        assert!(line.contains("case=qemu-smp1/system-test"));
        assert!(line.contains("phase=prepare-assets"));
    }

    #[test]
    fn axbuild_timing_grouped_c_compile_total_line_includes_mode() {
        let line = super::format_grouped_c_compile_total_line(
            "qemu-smp1/system",
            "per-subcase",
            Duration::from_millis(3456),
        );

        assert!(line.starts_with("AXBUILD_TIMING: "));
        assert!(line.contains("scope=grouped-c"));
        assert!(line.contains("case=qemu-smp1/system"));
        assert!(line.contains("phase=compile-total"));
        assert!(line.contains("mode=per-subcase"));
        assert!(line.contains("elapsed_s=3.456"));
    }

    #[test]
    fn axbuild_timing_grouped_c_root_project_compile_total_line_includes_mode() {
        let line = super::format_grouped_c_compile_total_line(
            "qemu-smp4/system",
            "root-project",
            Duration::from_millis(7890),
        );

        assert!(line.starts_with("AXBUILD_TIMING: "));
        assert!(line.contains("scope=grouped-c"));
        assert!(line.contains("case=qemu-smp4/system"));
        assert!(line.contains("phase=compile-total"));
        assert!(line.contains("mode=root-project"));
        assert!(line.contains("elapsed_s=7.890"));
    }
}
