use std::{
    env,
    ffi::{CStr, c_void},
    fs,
    mem::MaybeUninit,
    os::raw::{c_char, c_int, c_long},
    path::PathBuf,
    ptr, slice,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const UVC_FRAME_FORMAT_ANY: c_int = 0;
const UVC_FRAME_FORMAT_YUYV: c_int = 3;
const UVC_FRAME_FORMAT_MJPEG: c_int = 7;
const UVC_VS_FORMAT_UNCOMPRESSED: c_int = 4;
const UVC_VS_FORMAT_MJPEG: c_int = 6;

#[repr(C)]
struct UvcContext {
    _private: [u8; 0],
}

#[repr(C)]
struct UvcDevice {
    _private: [u8; 0],
}

#[repr(C)]
struct UvcDeviceHandle {
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TimeVal {
    tv_sec: c_long,
    tv_usec: c_long,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TimeSpec {
    tv_sec: c_long,
    tv_nsec: c_long,
}

#[repr(C)]
struct UvcFrame {
    data: *mut c_void,
    data_bytes: usize,
    width: u32,
    height: u32,
    frame_format: c_int,
    step: usize,
    sequence: u32,
    capture_time: TimeVal,
    capture_time_finished: TimeSpec,
    source: *mut UvcDeviceHandle,
    library_owns_data: u8,
    metadata: *mut c_void,
    metadata_bytes: usize,
}

#[repr(C)]
struct UvcStreamCtrl {
    bm_hint: u16,
    b_format_index: u8,
    b_frame_index: u8,
    dw_frame_interval: u32,
    w_key_frame_rate: u16,
    w_p_frame_rate: u16,
    w_comp_quality: u16,
    w_comp_window_size: u16,
    w_delay: u16,
    dw_max_video_frame_size: u32,
    dw_max_payload_transfer_size: u32,
    dw_clock_frequency: u32,
    bm_framing_info: u8,
    b_preferred_version: u8,
    b_min_version: u8,
    b_max_version: u8,
    b_interface_number: u8,
}

#[repr(C)]
struct UvcStreamingInterface {
    _private: [u8; 0],
}

#[repr(C)]
struct UvcStillFrameDesc {
    _private: [u8; 0],
}

#[repr(C)]
struct UvcFrameDesc {
    parent: *mut UvcFormatDesc,
    prev: *mut UvcFrameDesc,
    next: *mut UvcFrameDesc,
    descriptor_subtype: c_int,
    frame_index: u8,
    capabilities: u8,
    width: u16,
    height: u16,
    min_bit_rate: u32,
    max_bit_rate: u32,
    max_video_frame_buffer_size: u32,
    default_frame_interval: u32,
    min_frame_interval: u32,
    max_frame_interval: u32,
    frame_interval_step: u32,
    frame_interval_type: u8,
    bytes_per_line: u32,
    intervals: *mut u32,
}

#[repr(C)]
struct UvcFormatDesc {
    parent: *mut UvcStreamingInterface,
    prev: *mut UvcFormatDesc,
    next: *mut UvcFormatDesc,
    descriptor_subtype: c_int,
    format_index: u8,
    num_frame_descriptors: u8,
    guid_or_fourcc: [u8; 16],
    bits_per_pixel_or_flags: u8,
    default_frame_index: u8,
    aspect_ratio_x: u8,
    aspect_ratio_y: u8,
    interlace_flags: u8,
    copy_protect: u8,
    variable_size: u8,
    frame_descs: *mut UvcFrameDesc,
    still_frame_desc: *mut UvcStillFrameDesc,
}

#[link(name = "uvc")]
extern "C" {
    fn uvc_init(ctx: *mut *mut UvcContext, usb_ctx: *mut c_void) -> c_int;
    fn uvc_exit(ctx: *mut UvcContext);
    fn uvc_get_device_list(ctx: *mut UvcContext, list: *mut *mut *mut UvcDevice) -> c_int;
    fn uvc_free_device_list(list: *mut *mut UvcDevice, unref_devices: u8);
    fn uvc_ref_device(dev: *mut UvcDevice);
    fn uvc_unref_device(dev: *mut UvcDevice);
    fn uvc_get_bus_number(dev: *mut UvcDevice) -> u8;
    fn uvc_get_device_address(dev: *mut UvcDevice) -> u8;
    fn uvc_open(dev: *mut UvcDevice, devh: *mut *mut UvcDeviceHandle) -> c_int;
    fn uvc_close(devh: *mut UvcDeviceHandle);
    fn uvc_get_stream_ctrl_format_size(
        devh: *mut UvcDeviceHandle,
        ctrl: *mut UvcStreamCtrl,
        format: c_int,
        width: c_int,
        height: c_int,
        fps: c_int,
    ) -> c_int;
    fn uvc_get_format_descs(devh: *mut UvcDeviceHandle) -> *const UvcFormatDesc;
    fn uvc_start_streaming(
        devh: *mut UvcDeviceHandle,
        ctrl: *mut UvcStreamCtrl,
        cb: Option<extern "C" fn(*mut UvcFrame, *mut c_void)>,
        user_ptr: *mut c_void,
        flags: u8,
    ) -> c_int;
    fn uvc_stop_streaming(devh: *mut UvcDeviceHandle);
    fn uvc_strerror(err: c_int) -> *const c_char;
}

#[derive(Clone, Copy)]
enum FrameFormat {
    Any,
    Mjpeg,
    Yuyv,
}

impl FrameFormat {
    fn as_uvc(self) -> c_int {
        match self {
            Self::Any => UVC_FRAME_FORMAT_ANY,
            Self::Mjpeg => UVC_FRAME_FORMAT_MJPEG,
            Self::Yuyv => UVC_FRAME_FORMAT_YUYV,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Mjpeg => "mjpeg",
            Self::Yuyv => "yuyv",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "any" => Ok(Self::Any),
            "mjpeg" | "mjpg" => Ok(Self::Mjpeg),
            "yuyv" | "yuv" => Ok(Self::Yuyv),
            _ => Err(format!(
                "unsupported format `{value}`; expected any, mjpeg, or yuyv"
            )),
        }
    }
}

#[derive(Clone, Copy)]
struct VideoMode {
    format: FrameFormat,
    probe_format: FrameFormat,
    format_index: u8,
    frame_index: u8,
    width: c_int,
    height: c_int,
    fps: c_int,
    interval: u32,
    max_bit_rate: u32,
    max_frame_size: u32,
    estimated_bytes_per_sec: u64,
}

impl VideoMode {
    fn score(self) -> (u64, i32, c_int, c_int, c_int, u8, u8) {
        (
            self.estimated_bytes_per_sec,
            format_rank(self.format),
            self.width.saturating_mul(self.height),
            self.fps,
            self.width,
            self.format_index,
            self.frame_index,
        )
    }
}

struct Options {
    device: usize,
    format: FrameFormat,
    width: c_int,
    height: c_int,
    fps: c_int,
    auto_min_data: bool,
    list_modes: bool,
    interval: Duration,
    duration: Option<Duration>,
    max_frames: Option<u64>,
    save: SaveOptions,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            device: 0,
            format: FrameFormat::Mjpeg,
            width: 640,
            height: 480,
            fps: 30,
            auto_min_data: false,
            list_modes: false,
            interval: Duration::from_secs(1),
            duration: None,
            max_frames: None,
            save: SaveOptions::default(),
        }
    }
}

#[derive(Default)]
struct SaveOptions {
    dir: Option<PathBuf>,
    every: u64,
    max_saved: Option<u64>,
    last_only: bool,
}

struct SaveConfig {
    dir: PathBuf,
    every: u64,
    max_saved: Option<u64>,
    last_only: bool,
}

struct FrameCounters {
    frames: AtomicU64,
    bytes: AtomicU64,
    saved: AtomicU64,
    save_errors: AtomicU64,
    save: Option<SaveConfig>,
    last_frame: Mutex<Option<LastFrame>>,
}

impl Default for FrameCounters {
    fn default() -> Self {
        Self {
            frames: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            saved: AtomicU64::new(0),
            save_errors: AtomicU64::new(0),
            save: None,
            last_frame: Mutex::new(None),
        }
    }
}

struct LastFrame {
    frame_id: u64,
    sequence: u32,
    width: u32,
    height: u32,
    frame_format: c_int,
    data: Vec<u8>,
}

struct RunSummary {
    elapsed: Duration,
    frames: u64,
    bytes: u64,
    avg_fps: f64,
    avg_throughput_mib_s: f64,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("uvc-fps: error: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let options = parse_args()?;
    print_opening_options(&options);
    if let Some(save_dir) = &options.save.dir {
        fs::create_dir_all(save_dir)
            .map_err(|err| format!("failed to create save dir `{}`: {err}", save_dir.display()))?;
        println!(
            "uvc-fps: saving frames dir={} every={} max_saved={}",
            save_dir.display(),
            options.save.every.max(1),
            options
                .save
                .max_saved
                .map_or_else(|| "unlimited".to_string(), |value| value.to_string())
        );
        if options.save.last_only {
            println!("uvc-fps: save mode=last-frame");
        }
    }

    let mut ctx = ptr::null_mut();
    check_uvc(unsafe { uvc_init(&mut ctx, ptr::null_mut()) }, "uvc_init")?;
    let _ctx_guard = UvcContextGuard(ctx);

    let devh = unsafe { open_device(ctx, options.device)? };
    let _devh_guard = UvcDeviceHandleGuard(devh);

    let modes = unsafe { enumerate_modes(devh)? };
    if options.list_modes || options.auto_min_data {
        print_modes(&modes);
    }
    if options.list_modes {
        return Ok(());
    }

    let (mut ctrl, selected_mode) = if options.auto_min_data {
        select_min_data_mode(devh, &options, &modes)?
    } else {
        (
            probe_exact_mode(
                devh,
                options.format,
                options.width,
                options.height,
                options.fps,
            )?,
            None,
        )
    };
    if let Some(mode) = selected_mode {
        println!(
            "uvc-fps: selected mode format={} size={}x{} fps={} interval={} \
             estimated_bytes_per_sec={} bitrate={} max_frame={} format_index={} frame_index={}",
            mode.format.as_str(),
            mode.width,
            mode.height,
            mode.fps,
            mode.interval,
            mode.estimated_bytes_per_sec,
            mode.max_bit_rate,
            mode.max_frame_size,
            mode.format_index,
            mode.frame_index
        );
    }

    let counters = FrameCounters {
        save: options.save.dir.as_ref().map(|dir| SaveConfig {
            dir: dir.clone(),
            every: options.save.every.max(1),
            max_saved: options.save.max_saved,
            last_only: options.save.last_only,
        }),
        ..FrameCounters::default()
    };
    println!(
        "uvc-fps: entering uvc_start_streaming format_index={} frame_index={} interval={} \
         max_payload={} iface={}",
        ctrl.b_format_index,
        ctrl.b_frame_index,
        ctrl.dw_frame_interval,
        ctrl.dw_max_payload_transfer_size,
        ctrl.b_interface_number
    );
    let start_streaming_start = Instant::now();
    check_uvc(
        unsafe {
            uvc_start_streaming(
                devh,
                &mut ctrl,
                Some(frame_callback),
                &counters as *const FrameCounters as *mut c_void,
                0,
            )
        },
        "uvc_start_streaming",
    )?;
    let stream_guard = UvcStreamGuard(devh);

    println!(
        "uvc-fps: streaming started elapsed_ms={}",
        start_streaming_start.elapsed().as_millis()
    );
    let summary = report_loop(&options, &counters);
    drop(stream_guard);

    if let Some(save) = &counters.save {
        if save.last_only {
            save_last_frame(&counters, save)?;
        }
    }

    println!(
        "uvc-fps: done duration_sec={:.1} frames={} avg_fps={:.2} bytes={} saved={} \
         save_errors={} avg_throughput_mib_s={:.2}",
        summary.elapsed.as_secs_f64(),
        summary.frames,
        summary.avg_fps,
        summary.bytes,
        counters.saved.load(Ordering::Relaxed),
        counters.save_errors.load(Ordering::Relaxed),
        summary.avg_throughput_mib_s
    );
    Ok(())
}

fn print_opening_options(options: &Options) {
    if options.auto_min_data {
        println!(
            "uvc-fps: opening device={} format={} auto_min_data=1 interval_sec={}",
            options.device,
            options.format.as_str(),
            options.interval.as_secs()
        );
    } else {
        println!(
            "uvc-fps: opening device={} format={} size={}x{} fps={} interval_sec={}",
            options.device,
            options.format.as_str(),
            options.width,
            options.height,
            options.fps,
            options.interval.as_secs()
        );
    }
}

fn probe_exact_mode(
    devh: *mut UvcDeviceHandle,
    format: FrameFormat,
    width: c_int,
    height: c_int,
    fps: c_int,
) -> Result<UvcStreamCtrl, String> {
    let mut ctrl = MaybeUninit::<UvcStreamCtrl>::zeroed();
    println!(
        "uvc-fps: entering uvc_get_stream_ctrl_format_size format={} size={}x{} fps={}",
        format.as_str(),
        width,
        height,
        fps
    );
    let stream_ctrl_start = Instant::now();
    check_uvc(
        unsafe {
            uvc_get_stream_ctrl_format_size(
                devh,
                ctrl.as_mut_ptr(),
                format.as_uvc(),
                width,
                height,
                fps,
            )
        },
        "uvc_get_stream_ctrl_format_size",
    )?;
    let ctrl = unsafe { ctrl.assume_init() };
    println!(
        "uvc-fps: uvc_get_stream_ctrl_format_size returned elapsed_ms={} format_index={} \
         frame_index={} interval={} max_frame={} max_payload={} iface={}",
        stream_ctrl_start.elapsed().as_millis(),
        ctrl.b_format_index,
        ctrl.b_frame_index,
        ctrl.dw_frame_interval,
        ctrl.dw_max_video_frame_size,
        ctrl.dw_max_payload_transfer_size,
        ctrl.b_interface_number
    );
    Ok(ctrl)
}

unsafe fn enumerate_modes(devh: *mut UvcDeviceHandle) -> Result<Vec<VideoMode>, String> {
    let mut modes = Vec::new();
    let mut format_desc = unsafe { uvc_get_format_descs(devh) };
    while !format_desc.is_null() {
        let format = unsafe { &*format_desc };
        let frame_format = descriptor_frame_format(format);
        let mut frame_desc = format.frame_descs;
        while !frame_desc.is_null() {
            let frame = unsafe { &*frame_desc };
            for interval in frame_intervals(frame) {
                if interval == 0 {
                    continue;
                }
                let fps = interval_to_fps(interval);
                if fps <= 0 {
                    continue;
                }
                modes.push(VideoMode {
                    format: frame_format,
                    probe_format: probe_frame_format(format),
                    format_index: format.format_index,
                    frame_index: frame.frame_index,
                    width: frame.width as c_int,
                    height: frame.height as c_int,
                    fps,
                    interval,
                    max_bit_rate: frame.max_bit_rate,
                    max_frame_size: frame.max_video_frame_buffer_size,
                    estimated_bytes_per_sec: estimate_bytes_per_sec(frame, fps),
                });
            }
            frame_desc = frame.next;
        }
        format_desc = format.next;
    }

    if modes.is_empty() {
        return Err("camera did not report any UVC stream modes".to_string());
    }
    Ok(modes)
}

fn descriptor_frame_format(format: &UvcFormatDesc) -> FrameFormat {
    match format.descriptor_subtype {
        UVC_VS_FORMAT_MJPEG => FrameFormat::Mjpeg,
        UVC_VS_FORMAT_UNCOMPRESSED if is_yuyv_guid(&format.guid_or_fourcc) => FrameFormat::Yuyv,
        _ => FrameFormat::Any,
    }
}

fn probe_frame_format(format: &UvcFormatDesc) -> FrameFormat {
    match format.descriptor_subtype {
        UVC_VS_FORMAT_MJPEG => FrameFormat::Mjpeg,
        UVC_VS_FORMAT_UNCOMPRESSED => FrameFormat::Yuyv,
        _ => FrameFormat::Any,
    }
}

fn is_yuyv_guid(guid: &[u8; 16]) -> bool {
    &guid[..4] == b"YUY2"
}

fn frame_intervals(frame: &UvcFrameDesc) -> Vec<u32> {
    if frame.frame_interval_type == 0 {
        let min = frame.min_frame_interval;
        let max = frame.max_frame_interval;
        let step = frame.frame_interval_step;
        if min == 0 || max == 0 {
            return vec![frame.default_frame_interval];
        }
        if step == 0 {
            return vec![min, max];
        }
        let mut out = Vec::new();
        let mut interval = min;
        while interval <= max && out.len() < 64 {
            out.push(interval);
            let Some(next) = interval.checked_add(step) else {
                break;
            };
            interval = next;
        }
        out
    } else {
        let mut out = Vec::new();
        for index in 0..frame.frame_interval_type as usize {
            if frame.intervals.is_null() {
                break;
            }
            let interval = unsafe { *frame.intervals.add(index) };
            if interval == 0 {
                break;
            }
            out.push(interval);
        }
        if out.is_empty() {
            out.push(frame.default_frame_interval);
        }
        out
    }
}

fn interval_to_fps(interval: u32) -> c_int {
    if interval == 0 {
        return 0;
    }
    ((10_000_000u64 + interval as u64 / 2) / interval as u64)
        .max(1)
        .min(c_int::MAX as u64) as c_int
}

fn estimate_bytes_per_sec(frame: &UvcFrameDesc, fps: c_int) -> u64 {
    let by_bit_rate = (frame.max_bit_rate as u64).div_ceil(8);
    let by_frame_size = frame.max_video_frame_buffer_size.max(1) as u64 * fps.max(1) as u64;
    match (by_bit_rate, by_frame_size) {
        (0, value) => value,
        (value, 0) => value,
        (a, b) => a.max(b),
    }
}

fn print_modes(modes: &[VideoMode]) {
    println!("uvc-fps: modes count={}", modes.len());
    for mode in modes {
        println!(
            "uvc-fps: mode format={} size={}x{} fps={} interval={} estimated_bytes_per_sec={} \
             bitrate={} max_frame={} format_index={} frame_index={}",
            mode.format.as_str(),
            mode.width,
            mode.height,
            mode.fps,
            mode.interval,
            mode.estimated_bytes_per_sec,
            mode.max_bit_rate,
            mode.max_frame_size,
            mode.format_index,
            mode.frame_index
        );
    }
}

fn select_min_data_mode(
    devh: *mut UvcDeviceHandle,
    options: &Options,
    modes: &[VideoMode],
) -> Result<(UvcStreamCtrl, Option<VideoMode>), String> {
    let mut candidates: Vec<_> = modes
        .iter()
        .copied()
        .filter(|mode| format_matches(options.format, mode.format))
        .collect();
    if candidates.is_empty() {
        return Err(format!(
            "no UVC modes match requested format {}",
            options.format.as_str()
        ));
    }
    candidates.sort_by_key(|mode| mode.score());

    for mode in candidates {
        match probe_exact_mode(devh, mode.probe_format, mode.width, mode.height, mode.fps) {
            Ok(ctrl) if control_matches_mode(&ctrl, mode) => return Ok((ctrl, Some(mode))),
            Ok(ctrl) => {
                println!(
                    "uvc-fps: mode rejected format={} size={}x{} fps={} \
                     reason=negotiated-mismatch expected_format_index={} expected_frame_index={} \
                     expected_interval={} actual_format_index={} actual_frame_index={} \
                     actual_interval={} actual_max_frame={} actual_max_payload={}",
                    mode.format.as_str(),
                    mode.width,
                    mode.height,
                    mode.fps,
                    mode.format_index,
                    mode.frame_index,
                    mode.interval,
                    ctrl.b_format_index,
                    ctrl.b_frame_index,
                    ctrl.dw_frame_interval,
                    ctrl.dw_max_video_frame_size,
                    ctrl.dw_max_payload_transfer_size
                );
            }
            Err(err) => {
                println!(
                    "uvc-fps: mode rejected format={} size={}x{} fps={} error={}",
                    mode.format.as_str(),
                    mode.width,
                    mode.height,
                    mode.fps,
                    err
                );
            }
        }
    }

    Err("all auto-selected UVC modes failed probe".to_string())
}

fn control_matches_mode(ctrl: &UvcStreamCtrl, mode: VideoMode) -> bool {
    ctrl.b_format_index == mode.format_index
        && ctrl.b_frame_index == mode.frame_index
        && ctrl.dw_frame_interval == mode.interval
}

fn format_matches(requested: FrameFormat, actual: FrameFormat) -> bool {
    match requested {
        FrameFormat::Any => true,
        FrameFormat::Mjpeg => matches!(actual, FrameFormat::Mjpeg),
        FrameFormat::Yuyv => matches!(actual, FrameFormat::Yuyv | FrameFormat::Any),
    }
}

fn format_rank(format: FrameFormat) -> i32 {
    match format {
        FrameFormat::Mjpeg => 0,
        FrameFormat::Yuyv => 1,
        FrameFormat::Any => 2,
    }
}

unsafe fn open_device(
    ctx: *mut UvcContext,
    device_index: usize,
) -> Result<*mut UvcDeviceHandle, String> {
    let mut list = ptr::null_mut();
    println!("uvc-fps: entering uvc_get_device_list");
    let list_start = Instant::now();
    let list_result = unsafe { uvc_get_device_list(ctx, &mut list) };
    let list_elapsed = list_start.elapsed();
    check_uvc(list_result, "uvc_get_device_list")?;
    println!(
        "uvc-fps: uvc_get_device_list returned elapsed_ms={} list={:p}",
        list_elapsed.as_millis(),
        list
    );
    if list.is_null() {
        return Err("uvc_get_device_list returned null".to_string());
    }

    let dev = unsafe { *list.add(device_index) };
    if dev.is_null() {
        unsafe { uvc_free_device_list(list, 1) };
        return Err(format!("UVC device index {device_index} not found"));
    }

    unsafe { uvc_ref_device(dev) };
    let bus = unsafe { uvc_get_bus_number(dev) };
    let address = unsafe { uvc_get_device_address(dev) };
    unsafe { uvc_free_device_list(list, 1) };

    let mut devh = ptr::null_mut();
    println!(
        "uvc-fps: entering uvc_open device_index={} bus={} address={}",
        device_index, bus, address
    );
    let open_start = Instant::now();
    let open_result = unsafe { uvc_open(dev, &mut devh) };
    let open_elapsed = open_start.elapsed();
    unsafe { uvc_unref_device(dev) };
    check_uvc(open_result, "uvc_open")?;
    println!(
        "uvc-fps: uvc_open returned elapsed_ms={} bus={} address={} handle={:p}",
        open_elapsed.as_millis(),
        bus,
        address,
        devh
    );
    Ok(devh)
}

extern "C" fn frame_callback(frame: *mut UvcFrame, user_ptr: *mut c_void) {
    if frame.is_null() || user_ptr.is_null() {
        return;
    }

    let counters = unsafe { &*(user_ptr as *const FrameCounters) };
    let frame = unsafe { &*frame };
    let frame_id = counters.frames.fetch_add(1, Ordering::Relaxed) + 1;
    counters
        .bytes
        .fetch_add(frame.data_bytes as u64, Ordering::Relaxed);

    if let Some(save) = &counters.save {
        if save.last_only {
            cache_last_frame(counters, frame, frame_id);
            return;
        }
        save_frame(counters, save, frame, frame_id);
    }
}

fn cache_last_frame(counters: &FrameCounters, frame: &UvcFrame, frame_id: u64) {
    if frame.data.is_null() || frame.data_bytes == 0 {
        return;
    }

    let bytes = unsafe { slice::from_raw_parts(frame.data.cast::<u8>(), frame.data_bytes) };
    let Ok(mut last_frame) = counters.last_frame.lock() else {
        counters.save_errors.fetch_add(1, Ordering::Relaxed);
        return;
    };

    match last_frame.as_mut() {
        Some(last_frame) => {
            last_frame.frame_id = frame_id;
            last_frame.sequence = frame.sequence;
            last_frame.width = frame.width;
            last_frame.height = frame.height;
            last_frame.frame_format = frame.frame_format;
            last_frame.data.clear();
            last_frame.data.extend_from_slice(bytes);
        }
        None => {
            *last_frame = Some(LastFrame {
                frame_id,
                sequence: frame.sequence,
                width: frame.width,
                height: frame.height,
                frame_format: frame.frame_format,
                data: bytes.to_vec(),
            });
        }
    }
}

fn save_last_frame(counters: &FrameCounters, save: &SaveConfig) -> Result<(), String> {
    let last_frame = counters
        .last_frame
        .lock()
        .map_err(|_| "last-frame cache lock was poisoned".to_string())?
        .take()
        .ok_or_else(|| "no frame captured; cannot save final image".to_string())?;

    let Some(saved_id) = reserve_saved_id(counters, save.max_saved) else {
        return Ok(());
    };

    let extension = match last_frame.frame_format {
        UVC_FRAME_FORMAT_MJPEG => "jpg",
        _ => "raw",
    };
    let path = save.dir.join(format!("frame-{saved_id:06}.{extension}"));
    let bytes = last_frame.data.len();
    fs::write(&path, &last_frame.data)
        .map_err(|err| format!("{}: {err}", path.display()))
        .map_err(|err| {
            counters.save_errors.fetch_add(1, Ordering::Relaxed);
            format!("failed to save final frame: {err}")
        })?;

    println!(
        "uvc-fps: final frame saved id={} path={} bytes={} frame_id={} sequence={} size={}x{}",
        saved_id,
        path.display(),
        bytes,
        last_frame.frame_id,
        last_frame.sequence,
        last_frame.width,
        last_frame.height
    );
    Ok(())
}

fn save_frame(counters: &FrameCounters, save: &SaveConfig, frame: &UvcFrame, frame_id: u64) {
    if frame_id.checked_rem(save.every) != Some(0) {
        return;
    }
    if save
        .max_saved
        .is_some_and(|max_saved| counters.saved.load(Ordering::Relaxed) >= max_saved)
    {
        return;
    }
    if frame.data.is_null() || frame.data_bytes == 0 {
        return;
    }

    let Some(saved_id) = reserve_saved_id(counters, save.max_saved) else {
        return;
    };

    let extension = match frame.frame_format {
        UVC_FRAME_FORMAT_MJPEG => "jpg",
        _ => "raw",
    };
    let path = save.dir.join(format!("frame-{saved_id:06}.{extension}"));
    let bytes = unsafe { slice::from_raw_parts(frame.data.cast::<u8>(), frame.data_bytes) };

    match fs::write(&path, bytes) {
        Ok(()) => {
            println!(
                "uvc-fps: saved id={} path={} bytes={}",
                saved_id,
                path.display(),
                frame.data_bytes
            );
        }
        Err(err) => {
            let error_count = counters.save_errors.fetch_add(1, Ordering::Relaxed) + 1;
            if error_count <= 5 {
                eprintln!("uvc-fps: save error: {}: {err}", path.display());
            }
        }
    }
}

fn reserve_saved_id(counters: &FrameCounters, max_saved: Option<u64>) -> Option<u64> {
    if let Some(max_saved) = max_saved {
        counters
            .saved
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                (current < max_saved).then_some(current + 1)
            })
            .ok()
            .map(|previous| previous + 1)
    } else {
        Some(counters.saved.fetch_add(1, Ordering::Relaxed) + 1)
    }
}

fn report_loop(options: &Options, counters: &FrameCounters) -> RunSummary {
    let started = Instant::now();
    let mut last = started;
    let mut last_frames = 0;
    let mut last_bytes = 0;

    loop {
        let next_sleep = options
            .duration
            .map(|duration| {
                duration
                    .saturating_sub(started.elapsed())
                    .min(options.interval)
            })
            .unwrap_or(options.interval);
        if next_sleep.is_zero() {
            break;
        }
        thread::sleep(next_sleep);

        let now = Instant::now();
        let frames = counters.frames.load(Ordering::Relaxed);
        let bytes = counters.bytes.load(Ordering::Relaxed);
        let saved = counters.saved.load(Ordering::Relaxed);
        let save_errors = counters.save_errors.load(Ordering::Relaxed);
        let elapsed = now.duration_since(started).as_secs_f64();
        let interval = now.duration_since(last).as_secs_f64();
        let frame_delta = frames.saturating_sub(last_frames);
        let byte_delta = bytes.saturating_sub(last_bytes);
        let fps = frame_delta as f64 / interval.max(f64::EPSILON);
        let throughput_mib_s = byte_delta as f64 / interval.max(f64::EPSILON) / 1024.0 / 1024.0;

        println!(
            "uvc-fps: frames={} fps={:.2} bytes={} saved={} save_errors={} throughput_mib_s={:.2} \
             elapsed_sec={:.1}",
            frames, fps, bytes, saved, save_errors, throughput_mib_s, elapsed
        );

        if options
            .max_frames
            .is_some_and(|max_frames| frames >= max_frames)
            || options
                .duration
                .is_some_and(|duration| now.duration_since(started) >= duration)
        {
            break;
        }

        last = now;
        last_frames = frames;
        last_bytes = bytes;
    }

    let elapsed = started.elapsed();
    let frames = counters.frames.load(Ordering::Relaxed);
    let bytes = counters.bytes.load(Ordering::Relaxed);
    let elapsed_secs = elapsed.as_secs_f64().max(f64::EPSILON);
    RunSummary {
        elapsed,
        frames,
        bytes,
        avg_fps: frames as f64 / elapsed_secs,
        avg_throughput_mib_s: bytes as f64 / elapsed_secs / 1024.0 / 1024.0,
    }
}

fn parse_args() -> Result<Options, String> {
    let mut options = Options::default();
    let mut args = env::args().skip(1).peekable();

    while let Some(arg) = args.next() {
        if arg == "-h" || arg == "--help" {
            print_help();
            std::process::exit(0);
        }

        let (name, inline_value) = split_arg(&arg);
        match name {
            "--device" => {
                options.device = parse_value(name, inline_value, &mut args)?
                    .parse()
                    .map_err(|err| {
                        format!("invalid value for {name}: {err}; expected zero-based device index")
                    })?;
            }
            "--format" => {
                options.format = FrameFormat::parse(&parse_value(name, inline_value, &mut args)?)?;
            }
            "--width" => {
                options.width = parse_positive_i32(name, inline_value, &mut args)?;
            }
            "--height" => {
                options.height = parse_positive_i32(name, inline_value, &mut args)?;
            }
            "--fps" => {
                options.fps = parse_positive_i32(name, inline_value, &mut args)?;
            }
            "--auto-min-data" => {
                if inline_value.is_some() {
                    return Err("--auto-min-data does not take a value".to_string());
                }
                options.auto_min_data = true;
            }
            "--list-modes" => {
                if inline_value.is_some() {
                    return Err("--list-modes does not take a value".to_string());
                }
                options.list_modes = true;
            }
            "--interval-sec" => {
                let seconds = parse_value(name, inline_value, &mut args)?
                    .parse::<u64>()
                    .map_err(|err| format!("invalid value for {name}: {err}"))?;
                if seconds == 0 {
                    return Err(format!("{name} must be greater than zero"));
                }
                options.interval = Duration::from_secs(seconds);
            }
            "--duration-sec" => {
                let seconds = parse_value(name, inline_value, &mut args)?
                    .parse::<u64>()
                    .map_err(|err| format!("invalid value for {name}: {err}"))?;
                if seconds == 0 {
                    return Err(format!("{name} must be greater than zero"));
                }
                options.duration = Some(Duration::from_secs(seconds));
            }
            "--max-frames" => {
                let max_frames = parse_value(name, inline_value, &mut args)?
                    .parse::<u64>()
                    .map_err(|err| format!("invalid value for {name}: {err}"))?;
                if max_frames == 0 {
                    return Err(format!("{name} must be greater than zero"));
                }
                options.max_frames = Some(max_frames);
            }
            "--save-dir" => {
                options.save.dir = Some(PathBuf::from(parse_value(name, inline_value, &mut args)?));
            }
            "--save-every" => {
                options.save.every = parse_positive_u64(name, inline_value, &mut args)?;
            }
            "--max-saved" => {
                options.save.max_saved = Some(parse_positive_u64(name, inline_value, &mut args)?);
            }
            "--save-last" => {
                if inline_value.is_some() {
                    return Err("--save-last does not take a value".to_string());
                }
                options.save.last_only = true;
            }
            _ => return Err(format!("unknown argument `{arg}`")),
        }
    }

    if options.save.every == 0 {
        options.save.every = 1;
    }
    if options.save.max_saved.is_some() && options.save.dir.is_none() {
        return Err("--max-saved requires --save-dir".to_string());
    }
    if options.save.last_only && options.save.dir.is_none() {
        return Err("--save-last requires --save-dir".to_string());
    }

    Ok(options)
}

fn split_arg(arg: &str) -> (&str, Option<String>) {
    if let Some((name, value)) = arg.split_once('=') {
        (name, Some(value.to_string()))
    } else {
        (arg, None)
    }
}

fn parse_value<I>(
    name: &str,
    inline_value: Option<String>,
    args: &mut std::iter::Peekable<I>,
) -> Result<String, String>
where
    I: Iterator<Item = String>,
{
    if let Some(value) = inline_value {
        return Ok(value);
    }
    args.next()
        .ok_or_else(|| format!("{name} requires a value"))
}

fn parse_positive_i32<I>(
    name: &str,
    inline_value: Option<String>,
    args: &mut std::iter::Peekable<I>,
) -> Result<c_int, String>
where
    I: Iterator<Item = String>,
{
    let value = parse_value(name, inline_value, args)?
        .parse::<c_int>()
        .map_err(|err| format!("invalid value for {name}: {err}"))?;
    if value <= 0 {
        return Err(format!("{name} must be greater than zero"));
    }
    Ok(value)
}

fn parse_positive_u64<I>(
    name: &str,
    inline_value: Option<String>,
    args: &mut std::iter::Peekable<I>,
) -> Result<u64, String>
where
    I: Iterator<Item = String>,
{
    let value = parse_value(name, inline_value, args)?
        .parse::<u64>()
        .map_err(|err| format!("invalid value for {name}: {err}"))?;
    if value == 0 {
        return Err(format!("{name} must be greater than zero"));
    }
    Ok(value)
}

fn print_help() {
    println!(
        "uvc-fps\n\nUSAGE:\n  uvc-fps [OPTIONS]\n\nOPTIONS:\n  --device <INDEX>        Zero-based \
         UVC device index [default: 0]\n  --format <FORMAT>       any, mjpeg, or yuyv [default: \
         mjpeg]\n  --width <PIXELS>        Frame width [default: 640]\n  --height <PIXELS>       \
         Frame height [default: 480]\n  --fps <FPS>             Requested frame rate [default: \
         30]\n  --auto-min-data       Select the lowest-data probeable mode matching --format\n  \
         --list-modes          Print UVC descriptor modes and exit\n  --interval-sec <SECS>   Reporting interval [default: 1]\n  --duration-sec <SECS>   \
         Stop after this many seconds\n  --max-frames <N>        Stop after at least N frames\n  \
         --save-dir <DIR>        Save frames to DIR with incrementing file names\n  --save-last       \
         Cache frames while streaming, then save only the final frame\n  --save-every <N>        Save \
         every Nth frame when --save-dir is set [default: 1]\n  --max-saved <N>        Stop saving \
         after N saved frames\n  -h, --help              Print help"
    );
}

fn check_uvc(err: c_int, context: &str) -> Result<(), String> {
    if err >= 0 {
        return Ok(());
    }

    Err(format!("{context} failed: {}", uvc_error_message(err)))
}

fn uvc_error_message(err: c_int) -> String {
    let message = unsafe { uvc_strerror(err) };
    if message.is_null() {
        return format!("uvc_error_t({err})");
    }
    unsafe { CStr::from_ptr(message) }
        .to_string_lossy()
        .into_owned()
}

struct UvcContextGuard(*mut UvcContext);

impl Drop for UvcContextGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { uvc_exit(self.0) };
        }
    }
}

struct UvcDeviceHandleGuard(*mut UvcDeviceHandle);

impl Drop for UvcDeviceHandleGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { uvc_close(self.0) };
        }
    }
}

struct UvcStreamGuard(*mut UvcDeviceHandle);

impl Drop for UvcStreamGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { uvc_stop_streaming(self.0) };
        }
    }
}
