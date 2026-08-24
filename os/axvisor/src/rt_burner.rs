//! Opt-in host load used by the RT partition comparison benchmark.

use core::time::Duration;

use anyhow::{Result, bail};
use ax_std::os::arceos::modules::{ax_hal, ax_task};

const BOOTARG_PREFIX: &str = "rt_burner=";
const MAX_PHASE_MS: u64 = 60_000;
const MAX_START_DELAY_MS: u64 = 300_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BurnerConfig {
    cpu_id: usize,
    busy: Duration,
    idle: Duration,
    start_delay: Duration,
}

pub(crate) fn start() {
    let config = parse_bootargs(ax_hal::boot::bootargs(), ax_hal::cpu_num())
        .unwrap_or_else(|error| panic!("invalid RT burner configuration: {error:#}"));
    let Some(config) = config else {
        return;
    };

    std::thread::Builder::new()
        .name("rt-burner".into())
        .spawn(move || run(config))
        .unwrap_or_else(|error| panic!("failed to start RT burner: {error}"));
}

fn run(config: BurnerConfig) {
    let affinity = ax_task::AxCpuMask::one_shot(config.cpu_id);
    assert!(
        ax_task::set_current_affinity(affinity),
        "failed to pin RT burner to CPU {}",
        config.cpu_id
    );
    if !config.start_delay.is_zero() {
        info!(
            "RT_BURNER_ARMED cpu={} start_delay_ms={}",
            config.cpu_id,
            config.start_delay.as_millis(),
        );
        ax_task::sleep(config.start_delay);
    }
    info!(
        "RT_BURNER_READY cpu={} busy_ms={} idle_ms={} start_delay_ms={}",
        config.cpu_id,
        config.busy.as_millis(),
        config.idle.as_millis(),
        config.start_delay.as_millis(),
    );

    loop {
        ax_hal::time::busy_wait(config.busy);
        ax_task::sleep(config.idle);
    }
}

fn parse_bootargs(bootargs: Option<&str>, cpu_count: usize) -> Result<Option<BurnerConfig>> {
    let mut raw_config = None;
    for argument in bootargs.unwrap_or_default().split_ascii_whitespace() {
        let Some(value) = argument.strip_prefix(BOOTARG_PREFIX) else {
            continue;
        };
        if raw_config.replace(value).is_some() {
            bail!("{BOOTARG_PREFIX} may be specified only once");
        }
    }
    let Some(raw_config) = raw_config else {
        return Ok(None);
    };

    let mut fields = raw_config.split(':');
    let cpu_id = parse_field(fields.next(), "CPU")?;
    let busy_ms = parse_field(fields.next(), "busy duration")?;
    let idle_ms = parse_field(fields.next(), "idle duration")?;
    let start_delay_ms = match fields.next() {
        Some(value) => parse_field(Some(value), "start delay")?,
        None => 0,
    };
    if fields.next().is_some() {
        bail!("expected {BOOTARG_PREFIX}<cpu>:<busy_ms>:<idle_ms>[:<start_delay_ms>]");
    }
    if cpu_id >= cpu_count as u64 {
        bail!("CPU {cpu_id} is outside the {cpu_count}-CPU host");
    }
    if !(1..=MAX_PHASE_MS).contains(&busy_ms) {
        bail!("busy duration must be between 1 and {MAX_PHASE_MS} ms");
    }
    if !(1..=MAX_PHASE_MS).contains(&idle_ms) {
        bail!("idle duration must be between 1 and {MAX_PHASE_MS} ms");
    }
    if start_delay_ms > MAX_START_DELAY_MS {
        bail!("start delay must not exceed {MAX_START_DELAY_MS} ms");
    }

    Ok(Some(BurnerConfig {
        cpu_id: cpu_id as usize,
        busy: Duration::from_millis(busy_ms),
        idle: Duration::from_millis(idle_ms),
        start_delay: Duration::from_millis(start_delay_ms),
    }))
}

fn parse_field(field: Option<&str>, name: &str) -> Result<u64> {
    let Some(field) = field else {
        bail!("expected {BOOTARG_PREFIX}<cpu>:<busy_ms>:<idle_ms>");
    };
    field
        .parse()
        .map_err(|_| anyhow::anyhow!("{name} is not an unsigned integer: {field}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_burner_bootarg_disables_the_load() {
        assert_eq!(parse_bootargs(Some("console=ttyAMA0"), 4).unwrap(), None);
    }

    #[test]
    fn parses_a_bounded_busy_idle_load() {
        assert_eq!(
            parse_bootargs(Some("console=ttyAMA0 rt_burner=1:10:13"), 4).unwrap(),
            Some(BurnerConfig {
                cpu_id: 1,
                busy: Duration::from_millis(10),
                idle: Duration::from_millis(13),
                start_delay: Duration::ZERO,
            })
        );
    }

    #[test]
    fn parses_an_optional_start_delay() {
        assert_eq!(
            parse_bootargs(Some("rt_burner=1:10:53:60000"), 4).unwrap(),
            Some(BurnerConfig {
                cpu_id: 1,
                busy: Duration::from_millis(10),
                idle: Duration::from_millis(53),
                start_delay: Duration::from_millis(60_000),
            })
        );
    }

    #[test]
    fn rejects_an_offline_cpu() {
        let error = parse_bootargs(Some("rt_burner=4:10:10"), 4).unwrap_err();
        assert!(error.to_string().contains("outside the 4-CPU host"));
    }

    #[test]
    fn rejects_zero_length_phases() {
        assert!(parse_bootargs(Some("rt_burner=1:0:10"), 4).is_err());
        assert!(parse_bootargs(Some("rt_burner=1:10:0"), 4).is_err());
    }
}
