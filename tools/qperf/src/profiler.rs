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
use qemu_plugin::{
    CallbackFlags, PluginId, TranslationBlock, VCPUIndex,
    install::{Args, Info, Value},
    plugin::{HasCallbacks, Register},
    qemu_plugin_get_registers, qemu_plugin_read_memory_vaddr,
};
use zerocopy::IntoBytes;

use crate::reg::{AllRegs, Frame, Reg, Target};

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
    fn needs_registers(self) -> bool {
        matches!(self, Self::Fp)
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
            _ => bail!("invalid {name}: expected boolean"),
        })
        .transpose()
}

#[derive(Default)]
struct Stats {
    samples: AtomicU64,
    dropped_samples: AtomicU64,
    sample_failures: AtomicU64,
}

#[derive(Clone)]
pub struct Profiler {
    target: Target,
    tx: Sender<Vec<u64>>,
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
    last: Arc<Mutex<Instant>>,
    regs: Arc<AllRegs>,
    stats: Arc<Stats>,
}

impl Default for Profiler {
    fn default() -> Self {
        Self {
            target: Target::Riscv64,
            tx: bounded(0).0,
            intvl: Duration::MAX,
            max_depth: 128,
            mode: SamplingMode::Tb,
            filter_start: None,
            filter_end: None,
            filter_alias_start: None,
            filter_alias_end: None,
            filter_alias_offset: None,
            filter_kernel: false,
            callchain: CallchainMode::Leaf,
            last: Arc::new(Mutex::new(Instant::now())),
            regs: Arc::default(),
            stats: Arc::default(),
        }
    }
}

impl Profiler {
    fn sample(&mut self, ip: u64) -> qemu_plugin::Result<()> {
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
        if self.callchain == CallchainMode::Fp {
            let mut fp = self.regs.read(self.target.reg(Reg::Fp))?;
            let mut seen_fps = BTreeSet::new();

            while fp > 0 && fp % 8 == 0 && ips.len() < self.max_depth {
                if !seen_fps.insert(fp) {
                    break;
                }
                let mut frame = Frame::default();
                if qemu_plugin_read_memory_vaddr(fp - self.target.fp_offset(), frame.as_mut_bytes())
                    .is_err()
                {
                    break;
                };
                if qemu_plugin_read_memory_vaddr(frame.ip, &mut [0; 8]).is_err() {
                    break;
                }

                ips.push(self.canonicalize_ip(frame.ip).unwrap_or(frame.ip));
                if frame.fp <= fp {
                    break;
                }
                fp = frame.fp;
            }
        }

        match self.tx.try_send(ips) {
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

    fn sample_ip_for(&self, ip: u64) -> Option<u64> {
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

impl HasCallbacks for Profiler {
    fn on_vcpu_init(&mut self, _id: PluginId, _vcpu_id: VCPUIndex) -> qemu_plugin::Result<()> {
        if self.callchain.needs_registers() {
            self.regs = Arc::new(qemu_plugin_get_registers()?.into());
        }
        Ok(())
    }

    fn on_translation_block_translate(
        &mut self,
        _id: PluginId,
        tb: TranslationBlock,
    ) -> qemu_plugin::Result<()> {
        let Some(ip) = self.sample_ip_for(tb.vaddr()) else {
            return Ok(());
        };

        match self.mode {
            SamplingMode::Tb => {
                let mut this = self.clone();
                tb.register_execute_callback_flags(
                    move |_| {
                        if this.sample(ip).is_err() {
                            this.stats.sample_failures.fetch_add(1, Ordering::Relaxed);
                        }
                    },
                    callback_flags(self.callchain),
                );
            }
            SamplingMode::Insn => {
                tb.instructions().for_each(|insn| {
                    let Some(ip) = self.sample_ip_for(insn.vaddr()) else {
                        return;
                    };
                    let mut this = self.clone();
                    insn.register_execute_callback_flags(
                        move |_| {
                            if this.sample(ip).is_err() {
                                this.stats.sample_failures.fetch_add(1, Ordering::Relaxed);
                            }
                        },
                        callback_flags(self.callchain),
                    );
                });
            }
        }

        Ok(())
    }
}

fn callback_flags(callchain: CallchainMode) -> CallbackFlags {
    if callchain.needs_registers() {
        CallbackFlags::QEMU_PLUGIN_CB_R_REGS
    } else {
        CallbackFlags::QEMU_PLUGIN_CB_NO_REGS
    }
}

impl Register for Profiler {
    fn register(&mut self, id: PluginId, args: &Args, info: &Info) -> qemu_plugin::Result<()> {
        eprintln!("QPerf loaded: id={id:?} info={info:?}");
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
        let target = info.target_name.to_string();
        spawn(move || {
            while let Ok(event) = rx.recv() {
                if bincode::encode_into_std_write(event, &mut file, bincode::config::standard())
                    .is_err()
                {
                    writer_stats.sample_failures.fetch_add(1, Ordering::Relaxed);
                    break;
                }
            }
            let _ = file.flush();
            if let Ok(mut summary) = File::create(&summary_path).map(BufWriter::new) {
                let _ = writeln!(summary, "qperf_format_version = 1");
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
        });

        self.target = info.target_name.parse()?;
        self.tx = tx;
        self.intvl = Duration::from_secs_f64(1.0 / args.freq as f64);
        self.max_depth = args.max_depth;
        self.mode = args.mode;
        self.filter_start = args.filter_start;
        self.filter_end = args.filter_end;
        self.filter_alias_start = args.filter_alias_start;
        self.filter_alias_end = args.filter_alias_end;
        self.filter_alias_offset = args.filter_alias_offset;
        self.filter_kernel = args.filter_kernel;
        self.callchain = args.callchain;
        self.stats = stats;

        Ok(())
    }
}

qemu_plugin::register!(Profiler::default());
