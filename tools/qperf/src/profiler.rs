use std::{
    collections::BTreeSet,
    fs::File,
    io::{BufWriter, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::spawn,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use crossbeam_channel::{RecvTimeoutError, Sender, TrySendError, bounded};
use qemu_plugin::{
    CallbackFlags, PluginId, TranslationBlock, VCPUIndex,
    install::{Args, Info, Value},
    plugin::{HasCallbacks, Register},
    qemu_plugin_get_registers, qemu_plugin_read_memory_vaddr, qemu_plugin_register_atexit_cb,
};
use zerocopy::IntoBytes;

use crate::reg::{AllRegs, Frame, Reg, Target};

const MAX_FRAME_POINTER_DISTANCE: u64 = 16 * 1024 * 1024;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SamplingMode {
    Tb,
    Insn,
}

impl core::str::FromStr for SamplingMode {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "tb" => Ok(Self::Tb),
            "insn" => Ok(Self::Insn),
            _ => bail!("invalid sampling mode: {value}; expected 'tb' or 'insn'"),
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

impl core::str::FromStr for CallchainMode {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "leaf" => Ok(Self::Leaf),
            "fp" | "frame-pointer" | "frame_pointer" => Ok(Self::Fp),
            _ => bail!("invalid callchain mode: {value}; expected 'leaf' or 'fp'"),
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
            .map(|value| {
                if let Value::String(value) = value {
                    value.parse()
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
            .map(|value| {
                if let Value::String(value) = value {
                    value.parse()
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
        .map(|value| {
            if let Value::String(value) = value {
                u64::from_str_radix(value.trim_start_matches("0x").trim_start_matches("0X"), 16)
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

fn callback_flags(callchain: CallchainMode) -> CallbackFlags {
    if callchain.needs_registers() {
        CallbackFlags::QEMU_PLUGIN_CB_R_REGS
    } else {
        CallbackFlags::QEMU_PLUGIN_CB_NO_REGS
    }
}

fn update_max_atomic(value: &AtomicU64, candidate: u64) {
    let mut current = value.load(Ordering::Relaxed);
    while candidate > current {
        match value.compare_exchange_weak(current, candidate, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => break,
            Err(next) => current = next,
        }
    }
}

#[derive(Default)]
struct Stats {
    samples: AtomicU64,
    dropped_samples: AtomicU64,
    sample_failures: AtomicU64,
    samples_with_sp: AtomicU64,
    samples_with_fp: AtomicU64,
    unwind_success: AtomicU64,
    unwind_failed_no_fp: AtomicU64,
    unwind_failed_stack_read: AtomicU64,
    unwind_failed_return_ip_unread: AtomicU64,
    unwind_failed_non_monotonic_fp: AtomicU64,
    unwind_failed_loop: AtomicU64,
    total_frames: AtomicU64,
    max_observed_depth: AtomicU64,
    translated_blocks: AtomicU64,
    translated_instructions: AtomicU64,
    executed_blocks: AtomicU64,
    executed_instructions: AtomicU64,
    execute_callbacks: AtomicU64,
}

#[derive(Default)]
struct SelectedRegs {
    sp: Option<String>,
    fp: Option<String>,
    available: Vec<String>,
}

#[derive(Clone)]
pub struct Profiler {
    target: Target,
    tx: Sender<SampleRecord>,
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
    regs: Arc<AllRegs>,
    selected_regs: Arc<Mutex<SelectedRegs>>,
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
            started_at: Arc::new(Instant::now()),
            last: Arc::new(Mutex::new(Instant::now())),
            regs: Arc::default(),
            selected_regs: Arc::default(),
            stats: Arc::default(),
        }
    }
}

impl Profiler {
    fn sample(&mut self, cpu: VCPUIndex, ip: u64) -> qemu_plugin::Result<()> {
        let now = Instant::now();
        {
            let Ok(mut last) = self.last.try_lock() else {
                return Ok(());
            };
            if now.duration_since(*last) < self.intvl {
                return Ok(());
            }
            *last = now;
        }

        let mut ips = Vec::with_capacity(self.max_depth.min(16));
        ips.push(ip);
        let sp = if self.callchain.needs_registers() {
            self.regs
                .read_first(self.target.reg_candidates(Reg::Sp))
                .inspect(|_| {
                    self.stats.samples_with_sp.fetch_add(1, Ordering::Relaxed);
                })
                .map(|(_, value)| value)
                .unwrap_or(0)
        } else {
            0
        };
        let mut fp_value = 0;
        if self.callchain == CallchainMode::Fp {
            if !self.should_unwind_ip(ip) {
                return self.emit_sample(now, cpu, ip, sp, fp_value, ips);
            }
            let Ok((_, mut fp)) = self.regs.read_first(self.target.reg_candidates(Reg::Fp)) else {
                self.stats
                    .unwind_failed_no_fp
                    .fetch_add(1, Ordering::Relaxed);
                return self.emit_sample(now, cpu, ip, sp, fp_value, ips);
            };
            fp_value = fp;
            self.stats.samples_with_fp.fetch_add(1, Ordering::Relaxed);
            if !plausible_initial_frame_pointer(sp, fp) {
                self.stats
                    .unwind_failed_non_monotonic_fp
                    .fetch_add(1, Ordering::Relaxed);
                return self.emit_sample(now, cpu, ip, sp, fp_value, ips);
            }
            let mut seen_fps = BTreeSet::new();

            while fp >= self.target.fp_offset()
                && fp.is_multiple_of(8)
                && ips.len() < self.max_depth
            {
                if !seen_fps.insert(fp) {
                    self.stats
                        .unwind_failed_loop
                        .fetch_add(1, Ordering::Relaxed);
                    break;
                }
                let mut frame = Frame::default();
                if qemu_plugin_read_memory_vaddr(fp - self.target.fp_offset(), frame.as_mut_bytes())
                    .is_err()
                {
                    self.stats
                        .unwind_failed_stack_read
                        .fetch_add(1, Ordering::Relaxed);
                    break;
                };
                let Some(frame_ip) = self.canonicalize_unwind_ip(frame.ip) else {
                    self.stats
                        .unwind_failed_return_ip_unread
                        .fetch_add(1, Ordering::Relaxed);
                    break;
                };

                ips.push(frame_ip);
                if !plausible_next_frame_pointer(fp, frame.fp) {
                    self.stats
                        .unwind_failed_non_monotonic_fp
                        .fetch_add(1, Ordering::Relaxed);
                    break;
                }
                fp = frame.fp;
            }
            if ips.len() > 1 {
                self.stats.unwind_success.fetch_add(1, Ordering::Relaxed);
            }
        }

        self.emit_sample(now, cpu, ip, sp, fp_value, ips)
    }

    fn emit_sample(
        &mut self,
        now: Instant,
        cpu: VCPUIndex,
        ip: u64,
        sp: u64,
        fp: u64,
        ips: Vec<u64>,
    ) -> qemu_plugin::Result<()> {
        let depth = ips.len() as u64;
        self.stats.total_frames.fetch_add(depth, Ordering::Relaxed);
        update_max_atomic(&self.stats.max_observed_depth, depth);

        let elapsed_ns = now
            .duration_since(*self.started_at)
            .as_nanos()
            .min(u128::from(u64::MAX)) as u64;
        let record = SampleRecord {
            elapsed_ns,
            pc: ip,
            sp,
            fp,
            cpu,
            callchain: self.callchain.as_raw(),
            trace: ips,
        };

        match self.tx.try_send(record) {
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

    fn has_kernel_address_filter(&self) -> bool {
        self.filter_start.is_some() || self.filter_alias_start.is_some()
    }

    fn should_unwind_ip(&self, ip: u64) -> bool {
        !self.has_kernel_address_filter() || self.canonicalize_ip(ip).is_some()
    }

    fn canonicalize_unwind_ip(&self, ip: u64) -> Option<u64> {
        if self.has_kernel_address_filter() {
            self.canonicalize_ip(ip)
        } else {
            Some(ip)
        }
    }
}

fn plausible_initial_frame_pointer(sp: u64, fp: u64) -> bool {
    fp != 0
        && fp.is_multiple_of(8)
        && fp > sp
        && fp.saturating_sub(sp) <= MAX_FRAME_POINTER_DISTANCE
}

fn plausible_next_frame_pointer(current: u64, next: u64) -> bool {
    next.is_multiple_of(8)
        && next > current
        && next.saturating_sub(current) <= MAX_FRAME_POINTER_DISTANCE
}

impl HasCallbacks for Profiler {
    fn on_vcpu_init(&mut self, _id: PluginId, _vcpu_id: VCPUIndex) -> qemu_plugin::Result<()> {
        let regs = AllRegs::from(qemu_plugin_get_registers()?);
        let mut available = regs.names();
        available.sort();
        let selected = SelectedRegs {
            sp: regs.find_name(self.target.reg_candidates(Reg::Sp)),
            fp: regs.find_name(self.target.reg_candidates(Reg::Fp)),
            available,
        };
        eprintln!(
            "QPerf registers: sp={:?} fp={:?} available={}",
            selected.sp,
            selected.fp,
            selected.available.join(",")
        );
        self.regs = Arc::new(regs);
        if let Ok(mut shared) = self.selected_regs.lock() {
            *shared = selected;
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
                let insns = tb.size() as u64;
                self.stats.translated_blocks.fetch_add(1, Ordering::Relaxed);
                self.stats
                    .translated_instructions
                    .fetch_add(insns, Ordering::Relaxed);
                let stats = self.stats.clone();
                let mut this = self.clone();
                let flags = callback_flags(self.callchain);
                tb.register_execute_callback_flags(
                    move |cpu| {
                        stats.executed_blocks.fetch_add(1, Ordering::Relaxed);
                        stats
                            .executed_instructions
                            .fetch_add(insns, Ordering::Relaxed);
                        stats.execute_callbacks.fetch_add(1, Ordering::Relaxed);
                        if this.sample(cpu, ip).is_err() {
                            this.stats.sample_failures.fetch_add(1, Ordering::Relaxed);
                        }
                    },
                    flags,
                );
            }
            SamplingMode::Insn => {
                self.stats.translated_blocks.fetch_add(1, Ordering::Relaxed);
                tb.instructions().for_each(|insn| {
                    let Some(ip) = self.sample_ip_for(insn.vaddr()) else {
                        return;
                    };
                    self.stats
                        .translated_instructions
                        .fetch_add(1, Ordering::Relaxed);
                    let stats = self.stats.clone();
                    let mut this = self.clone();
                    let flags = callback_flags(self.callchain);
                    insn.register_execute_callback_flags(
                        move |cpu| {
                            stats.executed_instructions.fetch_add(1, Ordering::Relaxed);
                            stats.execute_callbacks.fetch_add(1, Ordering::Relaxed);
                            if this.sample(cpu, ip).is_err() {
                                this.stats.sample_failures.fetch_add(1, Ordering::Relaxed);
                            }
                        },
                        flags,
                    );
                });
            }
        }

        Ok(())
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
        let shutdown = Arc::new(AtomicBool::new(false));
        let writer_shutdown = shutdown.clone();
        let writer_done = Arc::new(AtomicBool::new(false));
        let writer_done_flag = writer_done.clone();
        let out = args.out.clone();
        let max_depth = args.max_depth;
        let freq = args.freq;
        let mode = args.mode;
        let filter_start = args.filter_start;
        let filter_end = args.filter_end;
        let filter_alias_start = args.filter_alias_start;
        let filter_alias_end = args.filter_alias_end;
        let filter_alias_offset = args.filter_alias_offset;
        let filter_kernel = args.filter_kernel;
        let callchain = args.callchain;
        let target = info.target_name.to_string();
        let selected_regs = self.selected_regs.clone();
        spawn(move || {
            loop {
                match rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(event) => {
                        if bincode::encode_into_std_write(
                            event,
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
                    Err(RecvTimeoutError::Timeout) => {
                        if writer_shutdown.load(Ordering::Acquire) {
                            break;
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            let _ = file.flush();
            if let Ok(mut summary) = File::create(&summary_path).map(BufWriter::new) {
                let selected_regs = selected_regs.lock().ok();
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
                    "selected_sp_register = {}",
                    selected_regs
                        .as_deref()
                        .and_then(|regs| regs.sp.as_deref())
                        .unwrap_or("unavailable")
                );
                let _ = writeln!(
                    summary,
                    "selected_fp_register = {}",
                    selected_regs
                        .as_deref()
                        .and_then(|regs| regs.fp.as_deref())
                        .unwrap_or("unavailable")
                );
                let _ = writeln!(
                    summary,
                    "available_registers = {}",
                    selected_regs
                        .as_deref()
                        .map(|regs| regs.available.join(","))
                        .unwrap_or_else(|| "unavailable".to_string())
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
                let samples = writer_stats.samples.load(Ordering::Relaxed);
                let total_frames = writer_stats.total_frames.load(Ordering::Relaxed);
                let avg_depth = if samples > 0 {
                    total_frames as f64 / samples as f64
                } else {
                    0.0
                };
                let _ = writeln!(
                    summary,
                    "samples_with_sp = {}",
                    writer_stats.samples_with_sp.load(Ordering::Relaxed)
                );
                let _ = writeln!(
                    summary,
                    "samples_with_fp = {}",
                    writer_stats.samples_with_fp.load(Ordering::Relaxed)
                );
                let _ = writeln!(
                    summary,
                    "unwind_success = {}",
                    writer_stats.unwind_success.load(Ordering::Relaxed)
                );
                let unwind_failed = writer_stats.unwind_failed_no_fp.load(Ordering::Relaxed)
                    + writer_stats
                        .unwind_failed_stack_read
                        .load(Ordering::Relaxed)
                    + writer_stats
                        .unwind_failed_return_ip_unread
                        .load(Ordering::Relaxed)
                    + writer_stats
                        .unwind_failed_non_monotonic_fp
                        .load(Ordering::Relaxed)
                    + writer_stats.unwind_failed_loop.load(Ordering::Relaxed);
                let _ = writeln!(summary, "unwind_failed = {unwind_failed}");
                let _ = writeln!(
                    summary,
                    "unwind_failed_no_fp = {}",
                    writer_stats.unwind_failed_no_fp.load(Ordering::Relaxed)
                );
                let _ = writeln!(
                    summary,
                    "unwind_failed_stack_read = {}",
                    writer_stats
                        .unwind_failed_stack_read
                        .load(Ordering::Relaxed)
                );
                let _ = writeln!(
                    summary,
                    "unwind_failed_return_ip_unread = {}",
                    writer_stats
                        .unwind_failed_return_ip_unread
                        .load(Ordering::Relaxed)
                );
                let _ = writeln!(
                    summary,
                    "unwind_failed_non_monotonic_fp = {}",
                    writer_stats
                        .unwind_failed_non_monotonic_fp
                        .load(Ordering::Relaxed)
                );
                let _ = writeln!(
                    summary,
                    "unwind_failed_loop = {}",
                    writer_stats.unwind_failed_loop.load(Ordering::Relaxed)
                );
                let _ = writeln!(summary, "total_frames = {total_frames}");
                let _ = writeln!(summary, "avg_stack_depth = {avg_depth:.6}");
                let _ = writeln!(
                    summary,
                    "max_observed_stack_depth = {}",
                    writer_stats.max_observed_depth.load(Ordering::Relaxed)
                );
                let _ = writeln!(
                    summary,
                    "translated_blocks = {}",
                    writer_stats.translated_blocks.load(Ordering::Relaxed)
                );
                let _ = writeln!(
                    summary,
                    "translated_instructions = {}",
                    writer_stats.translated_instructions.load(Ordering::Relaxed)
                );
                let _ = writeln!(
                    summary,
                    "executed_blocks = {}",
                    writer_stats.executed_blocks.load(Ordering::Relaxed)
                );
                let _ = writeln!(
                    summary,
                    "executed_instructions = {}",
                    writer_stats.executed_instructions.load(Ordering::Relaxed)
                );
                let _ = writeln!(
                    summary,
                    "execute_callbacks = {}",
                    writer_stats.execute_callbacks.load(Ordering::Relaxed)
                );
                let _ = writeln!(summary, "max_stack_depth = {max_depth}");
                let _ = writeln!(summary, "frequency_hz = {freq}");
                let _ = writeln!(summary, "sampling_mode = {mode:?}");
                if let (Some(start), Some(end)) = (filter_start, filter_end) {
                    let _ = writeln!(summary, "filter_start = 0x{start:x}");
                    let _ = writeln!(summary, "filter_end = 0x{end:x}");
                }
                if let (Some(start), Some(end), Some(offset)) =
                    (filter_alias_start, filter_alias_end, filter_alias_offset)
                {
                    let _ = writeln!(summary, "filter_alias_start = 0x{start:x}");
                    let _ = writeln!(summary, "filter_alias_end = 0x{end:x}");
                    let _ = writeln!(summary, "filter_alias_offset = 0x{offset:x}");
                }
                let _ = writeln!(summary, "filter_kernel = {filter_kernel}");
                let _ = writeln!(summary, "arch = {target}");
                let _ = writeln!(summary, "output = {}", out.display());
                let _ = summary.flush();
            }
            writer_done_flag.store(true, Ordering::Release);
        });
        qemu_plugin_register_atexit_cb(id, move |_| {
            shutdown.store(true, Ordering::Release);
            let deadline = Instant::now() + Duration::from_secs(2);
            while !writer_done.load(Ordering::Acquire) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
        })?;

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
        self.started_at = Arc::new(Instant::now());
        self.last = Arc::new(Mutex::new(Instant::now()));
        self.stats = stats;

        Ok(())
    }
}

qemu_plugin::register!(Profiler::default());
