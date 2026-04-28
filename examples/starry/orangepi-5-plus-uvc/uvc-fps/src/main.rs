use std::{
    env,
    ffi::{CStr, c_void},
    mem::MaybeUninit,
    os::raw::{c_char, c_int, c_long},
    ptr,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

const UVC_FRAME_FORMAT_ANY: c_int = 0;
const UVC_FRAME_FORMAT_YUYV: c_int = 3;
const UVC_FRAME_FORMAT_MJPEG: c_int = 7;

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

struct Options {
    device: usize,
    format: FrameFormat,
    width: c_int,
    height: c_int,
    fps: c_int,
    interval: Duration,
    max_frames: Option<u64>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            device: 0,
            format: FrameFormat::Mjpeg,
            width: 640,
            height: 480,
            fps: 30,
            interval: Duration::from_secs(1),
            max_frames: None,
        }
    }
}

#[derive(Default)]
struct FrameCounters {
    frames: AtomicU64,
    bytes: AtomicU64,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("uvc-fps: error: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let options = parse_args()?;
    println!(
        "uvc-fps: opening device={} format={} size={}x{} fps={} interval_sec={}",
        options.device,
        options.format.as_str(),
        options.width,
        options.height,
        options.fps,
        options.interval.as_secs()
    );

    let mut ctx = ptr::null_mut();
    check_uvc(unsafe { uvc_init(&mut ctx, ptr::null_mut()) }, "uvc_init")?;
    let _ctx_guard = UvcContextGuard(ctx);

    let devh = unsafe { open_device(ctx, options.device)? };
    let _devh_guard = UvcDeviceHandleGuard(devh);

    let mut ctrl = MaybeUninit::<UvcStreamCtrl>::zeroed();
    check_uvc(
        unsafe {
            uvc_get_stream_ctrl_format_size(
                devh,
                ctrl.as_mut_ptr(),
                options.format.as_uvc(),
                options.width,
                options.height,
                options.fps,
            )
        },
        "uvc_get_stream_ctrl_format_size",
    )?;
    let mut ctrl = unsafe { ctrl.assume_init() };

    let counters = FrameCounters::default();
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
    let _stream_guard = UvcStreamGuard(devh);

    println!("uvc-fps: streaming started");
    report_loop(&options, &counters);
    Ok(())
}

unsafe fn open_device(
    ctx: *mut UvcContext,
    device_index: usize,
) -> Result<*mut UvcDeviceHandle, String> {
    let mut list = ptr::null_mut();
    check_uvc(
        unsafe { uvc_get_device_list(ctx, &mut list) },
        "uvc_get_device_list",
    )?;
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
    let open_result = unsafe { uvc_open(dev, &mut devh) };
    unsafe { uvc_unref_device(dev) };
    check_uvc(open_result, "uvc_open")?;
    println!("uvc-fps: opened bus={} address={}", bus, address);
    Ok(devh)
}

extern "C" fn frame_callback(frame: *mut UvcFrame, user_ptr: *mut c_void) {
    if frame.is_null() || user_ptr.is_null() {
        return;
    }

    let counters = unsafe { &*(user_ptr as *const FrameCounters) };
    let frame = unsafe { &*frame };
    counters.frames.fetch_add(1, Ordering::Relaxed);
    counters
        .bytes
        .fetch_add(frame.data_bytes as u64, Ordering::Relaxed);
}

fn report_loop(options: &Options, counters: &FrameCounters) {
    let started = Instant::now();
    let mut last = started;
    let mut last_frames = 0;
    let mut last_bytes = 0;

    loop {
        thread::sleep(options.interval);

        let now = Instant::now();
        let frames = counters.frames.load(Ordering::Relaxed);
        let bytes = counters.bytes.load(Ordering::Relaxed);
        let elapsed = now.duration_since(started).as_secs_f64();
        let interval = now.duration_since(last).as_secs_f64();
        let frame_delta = frames.saturating_sub(last_frames);
        let byte_delta = bytes.saturating_sub(last_bytes);
        let fps = frame_delta as f64 / interval.max(f64::EPSILON);
        let throughput_mib_s = byte_delta as f64 / interval.max(f64::EPSILON) / 1024.0 / 1024.0;

        println!(
            "uvc-fps: frames={} fps={:.2} bytes={} throughput_mib_s={:.2} elapsed_sec={:.1}",
            frames, fps, bytes, throughput_mib_s, elapsed
        );

        if options
            .max_frames
            .is_some_and(|max_frames| frames >= max_frames)
        {
            break;
        }

        last = now;
        last_frames = frames;
        last_bytes = bytes;
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
            "--interval-sec" => {
                let seconds = parse_value(name, inline_value, &mut args)?
                    .parse::<u64>()
                    .map_err(|err| format!("invalid value for {name}: {err}"))?;
                if seconds == 0 {
                    return Err(format!("{name} must be greater than zero"));
                }
                options.interval = Duration::from_secs(seconds);
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
            _ => return Err(format!("unknown argument `{arg}`")),
        }
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

fn print_help() {
    println!(
        "uvc-fps\n\nUSAGE:\n  uvc-fps [OPTIONS]\n\nOPTIONS:\n  --device <INDEX>        Zero-based \
         UVC device index [default: 0]\n  --format <FORMAT>       any, mjpeg, or yuyv [default: \
         mjpeg]\n  --width <PIXELS>        Frame width [default: 640]\n  --height <PIXELS>       \
         Frame height [default: 480]\n  --fps <FPS>             Requested frame rate [default: \
         30]\n  --interval-sec <SECS>   Reporting interval [default: 1]\n  --max-frames <N>        \
         Stop after at least N frames\n  -h, --help              Print help"
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
