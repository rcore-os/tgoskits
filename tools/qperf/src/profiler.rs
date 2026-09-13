use std::{
    collections::BTreeSet,
    fs::File,
    io::{BufWriter, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread::spawn,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use crossbeam_channel::{Sender, TrySendError, bounded};
use zerocopy::IntoBytes;

use crate::{
    qemu::{Args, Value},
    reg,
    target::{Frame, Reg, Target},
};

#[derive(bincode::Encode)]
struct SampleRecord {
    elapsed_ns: u64,
    pc: u64,
    sp: u64,
    fp: u64,
    cpu: u32,
    callchain: u8,
    trace: Vec<u64>,
}

enum WriterEvent {
    Sample(SampleRecord),
    Shutdown(Sender<()>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SamplingMode {
    Tb,
    Insn,
}

impl std::str::FromStr for SamplingMode {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "tb" => Ok(SamplingMode::Tb),
            "insn" => Ok(SamplingMode::Insn),
            _ => bail!("invalid sampling mode: {s} (expected 'tb' or 'insn')"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallchainMode {
    Leaf,
    Fp,
}

impl CallchainMode {
    fn as_raw(self) -> u8 {
        match self {
            Self::Leaf => 0,
            Self::Fp => 1,
        }
    }

    fn needs_registers(self) -> bool {
        matches!(self, Self::Fp)
    }
}

impl core::fmt::Display for CallchainMode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Leaf => "leaf",
            Self::Fp => "fp",
        })
    }
}

impl std::str::FromStr for CallchainMode {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "leaf" => Ok(Self::Leaf),
            "fp" | "frame-pointer" | "frame_pointer" => Ok(Self::Fp),
            _ => bail!("invalid callchain mode: {s} (expected 'leaf' or 'fp')"),
        }
    }
}

#[derive(Debug)]
struct PluginArgs {
    freq: u32,
    out: PathBuf,
    max_depth: usize,
    queue_size: usize,
    mode: SamplingMode,
    filter_start: Option<u64>,
    filter_end: Option<u64>,
    filter_alias_start: Option<u64>,
    filter_alias_end: Option<u64>,
    filter_alias_offset: Option<u64>,
    filter_kernel: bool,
    callchain: CallchainMode,
}

impl TryFrom<&Args> for PluginArgs {
    type Error = anyhow::Error;

    fn try_from(args: &Args) -> Result<Self, Self::Error> {
        let freq = args
            .parsed
            .get("freq")
            .map(|v| {
                if let Value::Integer(v) = v
                    && let Ok(v) = (*v).try_into()
                {
                    Ok(v)
                } else {
                    bail!("invalid frequency")
                }
            })
            .transpose()?
            .unwrap_or(99);
        let out = args
            .parsed
            .get("out")
            .map(|s| {
                if let Value::String(s) = s {
                    Ok(s.into())
                } else {
                    bail!("invalid output path")
                }
            })
            .transpose()?
            .unwrap_or("qperf.bin".into());
        if freq == 0 {
            bail!("frequency must be greater than 0");
        }
        let max_depth = parse_usize_arg(args, "max_depth")?.unwrap_or(128);
        let queue_size = parse_usize_arg(args, "queue_size")?.unwrap_or(4096);
        let mode = args
            .parsed
            .get("mode")
            .map(|v| {
                if let Value::String(s) = v {
                    s.parse::<SamplingMode>()
                } else {
                    bail!("invalid mode")
                }
            })
            .transpose()?
            .unwrap_or(SamplingMode::Tb);
        let filter_start = parse_u64_hex_arg(args, "filter_start")?;
        let filter_end = parse_u64_hex_arg(args, "filter_end")?;
        let filter_alias_start = parse_u64_hex_arg(args, "filter_alias_start")?;
        let filter_alias_end = parse_u64_hex_arg(args, "filter_alias_end")?;
        let filter_alias_offset = parse_u64_hex_arg(args, "filter_alias_offset")?;
        let filter_kernel = parse_bool_arg(args, "filter_kernel")?
            .unwrap_or(filter_start.is_some() || filter_alias_start.is_some());
        let callchain = args
            .parsed
            .get("callchain")
            .map(|v| {
                if let Value::String(s) = v {
                    s.parse::<CallchainMode>()
                } else {
                    bail!("invalid callchain")
                }
            })
            .transpose()?
            .unwrap_or(CallchainMode::Leaf);
        if max_depth == 0 {
            bail!("max_depth must be greater than 0");
        }
        if queue_size == 0 {
            bail!("queue_size must be greater than 0");
        }
        if filter_start.is_some() != filter_end.is_some() {
            bail!("filter_start and filter_end must be provided together");
        }
        if matches!((filter_start, filter_end), (Some(start), Some(end)) if start >= end) {
            bail!("filter_start must be less than filter_end");
        }
        let alias_count = [
            filter_alias_start.is_some(),
            filter_alias_end.is_some(),
            filter_alias_offset.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count();
        if alias_count != 0 && alias_count != 3 {
            bail!(
                "filter_alias_start, filter_alias_end, and filter_alias_offset must be provided \
                 together"
            );
        }
        if matches!((filter_alias_start, filter_alias_end), (Some(start), Some(end)) if start >= end)
        {
            bail!("filter_alias_start must be less than filter_alias_end");
        }
        Ok(PluginArgs {
            freq,
            out,
            max_depth,
            queue_size,
            mode,
            filter_start,
            filter_end,
            filter_alias_start,
            filter_alias_end,
            filter_alias_offset,
            filter_kernel,
            callchain,
        })
    }
}

fn parse_usize_arg(args: &Args, name: &str) -> anyhow::Result<Option<usize>> {
    args.parsed
        .get(name)
        .map(|v| {
            if let Value::Integer(v) = v
                && let Ok(v) = (*v).try_into()
            {
                Ok(v)
            } else {
                bail!("invalid {name}")
            }
        })
        .transpose()
}

fn parse_u64_hex_arg(args: &Args, name: &str) -> anyhow::Result<Option<u64>> {
    args.parsed
        .get(name)
        .map(|v| {
            if let Value::String(s) = v {
                u64::from_str_radix(s.trim_start_matches("0x").trim_start_matches("0X"), 16)
                    .with_context(|| format!("invalid {name}: expected hex address"))
            } else {
                bail!("invalid {name}: expected hex string")
            }
        })
        .transpose()
}

fn parse_bool_arg(args: &Args, name: &str) -> anyhow::Result<Option<bool>> {
    args.parsed
        .get(name)
        .map(|value| match value {
            Value::Integer(value) => Ok(*value != 0),
            Value::String(value) => match value.to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => Ok(true),
                "0" | "false" | "no" | "off" => Ok(false),
                _ => bail!("invalid {name}: expected boolean"),
            },
        })
        .transpose()
}

#[derive(Default)]
struct Stats {
    samples: AtomicU64,
    dropped_samples: AtomicU64,
    sample_failures: AtomicU64,
}

pub struct Profiler {
    target: Target,
    tx: Sender<WriterEvent>,
    intvl: Duration,
    max_depth: usize,
    mode: SamplingMode,
    filter_start: Option<u64>,
    filter_end: Option<u64>,
    filter_alias_start: Option<u64>,
    filter_alias_end: Option<u64>,
    filter_alias_offset: Option<u64>,
    filter_kernel: bool,
    callchain: CallchainMode,
    started_at: Arc<Instant>,
    last: Arc<Mutex<Instant>>,
    stats: Arc<Stats>,
}

impl Profiler {
    /// # Safety
    /// Call only on an initialized vCPU inside a QEMU execution callback, with
    /// R_REGS enabled when using frame-pointer sampling.
    pub unsafe fn sample(&self, cpu: u32, ip: u64) -> anyhow::Result<()> {
        let now = Instant::now();
        let Ok(mut last) = self.last.try_lock() else {
            return Ok(());
        };
        if now.duration_since(*last) < self.intvl {
            return Ok(());
        }
        *last = now;

        let mut ips = Vec::with_capacity(self.max_depth.min(16));
        ips.push(ip);
        let sp = if self.callchain.needs_registers() {
            // SAFETY: The execution callback runs in this vCPU's R_REGS context.
            unsafe { reg::read(cpu, self.target.reg(Reg::Sp)) }?
        } else {
            0
        };
        let mut fp_value = 0;
        if self.callchain == CallchainMode::Fp {
            // SAFETY: Same initialized vCPU and R_REGS context as the SP read.
            let mut fp = unsafe { reg::read(cpu, self.target.reg(Reg::Fp)) }?;
            fp_value = fp;
            let mut seen_fps = BTreeSet::new();

            while fp > 0 && fp % 8 == 0 && ips.len() < self.max_depth {
                if !seen_fps.insert(fp) {
                    break;
                }
                let Some(frame_address) = self.target.frame_address(fp) else {
                    break;
                };
                let mut frame = Frame::default();
                // SAFETY: This callback has the active vCPU memory context. QEMU
                // validates guest mappings and reports an unreadable frame.
                if unsafe { reg::read_memory(frame_address, frame.as_mut_bytes()) }.is_err() {
                    break;
                };
                // SAFETY: Same vCPU context; QEMU validates this candidate PC.
                if unsafe { reg::read_memory(frame.ip, &mut [0; 8]) }.is_err() {
                    break;
                }

                let Some(frame_ip) = self.sample_ip_for(frame.ip) else {
                    break;
                };
                ips.push(frame_ip);
                if frame.fp <= fp {
                    break;
                }
                fp = frame.fp;
            }
        }

        let elapsed_ns = now
            .duration_since(*self.started_at)
            .as_nanos()
            .min(u128::from(u64::MAX)) as u64;
        let record = SampleRecord {
            elapsed_ns,
            pc: ip,
            sp,
            fp: fp_value,
            cpu,
            callchain: self.callchain.as_raw(),
            trace: ips,
        };

        match self.tx.try_send(WriterEvent::Sample(record)) {
            Ok(()) => {
                self.stats.samples.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Full(_)) => {
                self.stats.dropped_samples.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => {
                self.stats.sample_failures.fetch_add(1, Ordering::Relaxed);
            }
        }

        Ok(())
    }

    pub fn sample_ip_for(&self, ip: u64) -> Option<u64> {
        if let Some(mapped) = self.canonicalize_ip(ip) {
            return Some(mapped);
        }
        if self.filter_kernel {
            return None;
        }
        Some(ip)
    }

    fn canonicalize_ip(&self, ip: u64) -> Option<u64> {
        if let (Some(start), Some(end)) = (self.filter_start, self.filter_end)
            && ip >= start
            && ip < end
        {
            return Some(ip);
        }
        if let (Some(start), Some(end), Some(offset)) = (
            self.filter_alias_start,
            self.filter_alias_end,
            self.filter_alias_offset,
        ) && ip >= start
            && ip < end
        {
            return Some(ip.wrapping_add(offset));
        }
        None
    }
}

impl Profiler {
    pub fn needs_registers(&self) -> bool {
        self.callchain.needs_registers()
    }

    pub fn instruction_sampling(&self) -> bool {
        self.mode == SamplingMode::Insn
    }

    pub fn record_failure(&self) {
        self.stats.sample_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn start(args: &Args, target_name: &str) -> anyhow::Result<Self> {
        let target_arch = target_name.parse()?;
        let args = PluginArgs::try_from(args)?;
        eprintln!("QPerf arguments: {args:?}");
        let summary_path = args.out.with_extension("summary.txt");
        let file = File::create(&args.out).context("Failed to create output file")?;
        let mut file = BufWriter::new(file);

        let (tx, rx) = bounded(args.queue_size);
        let stats = Arc::<Stats>::default();
        let writer_stats = stats.clone();
        let out = args.out.clone();
        let max_depth = args.max_depth;
        let freq = args.freq;
        let callchain = args.callchain;
        let target = target_name.to_string();
        spawn(move || {
            let mut shutdown = None;
            while let Ok(event) = rx.recv() {
                match event {
                    WriterEvent::Sample(sample) => {
                        if bincode::encode_into_std_write(
                            sample,
                            &mut file,
                            bincode::config::standard(),
                        )
                        .is_err()
                        {
                            writer_stats.sample_failures.fetch_add(1, Ordering::Relaxed);
                            break;
                        }
                        let _ = file.flush();
                    }
                    WriterEvent::Shutdown(done) => {
                        shutdown = Some(done);
                        break;
                    }
                }
            }
            let _ = file.flush();
            if let Ok(mut summary) = File::create(&summary_path).map(BufWriter::new) {
                let _ = writeln!(summary, "qperf_format_version = 3");
                let _ = writeln!(summary, "record_timestamp = elapsed_ns");
                let _ = writeln!(
                    summary,
                    "record_fields = elapsed_ns,pc,sp,fp,cpu,callchain,trace"
                );
                let _ = writeln!(summary, "callchain_method = {callchain}");
                let _ = writeln!(
                    summary,
                    "callchain_enabled = {}",
                    callchain.needs_registers()
                );
                let _ = writeln!(
                    summary,
                    "samples = {}",
                    writer_stats.samples.load(Ordering::Relaxed)
                );
                let _ = writeln!(
                    summary,
                    "dropped_samples = {}",
                    writer_stats.dropped_samples.load(Ordering::Relaxed)
                );
                let _ = writeln!(
                    summary,
                    "sample_failures = {}",
                    writer_stats.sample_failures.load(Ordering::Relaxed)
                );
                let _ = writeln!(summary, "max_stack_depth = {max_depth}");
                let _ = writeln!(summary, "frequency_hz = {freq}");
                let _ = writeln!(summary, "arch = {target}");
                let _ = writeln!(summary, "output = {}", out.display());
                let _ = summary.flush();
            }
            if let Some(done) = shutdown {
                let _ = done.send(());
            }
        });

        Ok(Self {
            target: target_arch,
            tx,
            intvl: Duration::from_secs_f64(1.0 / args.freq as f64),
            max_depth: args.max_depth,
            mode: args.mode,
            filter_start: args.filter_start,
            filter_end: args.filter_end,
            filter_alias_start: args.filter_alias_start,
            filter_alias_end: args.filter_alias_end,
            filter_alias_offset: args.filter_alias_offset,
            filter_kernel: args.filter_kernel,
            callchain: args.callchain,
            started_at: Arc::new(Instant::now()),
            last: Arc::new(Mutex::new(Instant::now())),
            stats,
        })
    }
}

impl Drop for Profiler {
    fn drop(&mut self) {
        let (done_tx, done_rx) = bounded(0);
        if self.tx.send(WriterEvent::Shutdown(done_tx)).is_ok() {
            let _ = done_rx.recv();
        }
    }
}
