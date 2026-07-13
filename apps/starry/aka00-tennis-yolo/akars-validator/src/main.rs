mod camera;
mod detector;
mod image_bridge;
mod tpu;

use std::{
    env, fmt, fs,
    path::{Path, PathBuf},
    time::Instant,
};

use crate::{
    camera::CameraFrame,
    detector::Detection,
    tpu::{InferTiming, InferenceConfig, open_model},
};

const DEFAULT_BBOX_TOLERANCE_PX: i32 = 12;
const DEFAULT_SCORE_TOLERANCE_Q10000: i32 = 800;

#[derive(Debug)]
struct Cli {
    model: PathBuf,
    image_list: PathBuf,
    expected: PathBuf,
    classes: i32,
    conf: f32,
    iou: f32,
    write_expected: bool,
    warmup: usize,
    repeat: usize,
}

#[derive(Clone, Debug)]
struct InputImage {
    path: String,
    frame: CameraFrame,
}

#[derive(Clone, Debug)]
struct ImageExpected {
    index: usize,
    path: String,
    detections: Vec<ExpectedDetection>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ExpectedDetection {
    cls: i32,
    score_q10000: i32,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[derive(Clone, Debug)]
struct ExpectedSet {
    images: Vec<ImageExpected>,
    bbox_tolerance_px: i32,
    score_tolerance_q10000: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TimingStats {
    average_us: i64,
    p50_us: i64,
    p95_us: i64,
}

#[derive(Default)]
struct BenchmarkSamples {
    decode_us: Vec<i64>,
    resize_us: Vec<i64>,
    preprocess_us: Vec<i64>,
    forward_us: Vec<i64>,
    postprocess_us: Vec<i64>,
    total_us: Vec<i64>,
}

impl BenchmarkSamples {
    fn push(&mut self, timing: InferTiming, total_us: i64) {
        self.decode_us.push(timing.decode_us);
        self.resize_us.push(timing.resize_us);
        self.preprocess_us.push(timing.preprocess_us);
        self.forward_us.push(timing.forward_us);
        self.postprocess_us.push(timing.postprocess_us);
        self.total_us.push(total_us);
    }

    fn format_result(&self, measured_runs: usize, images: usize) -> String {
        let decode = timing_stats(&self.decode_us).expect("benchmark has decode samples");
        let resize = timing_stats(&self.resize_us).expect("benchmark has resize samples");
        let preprocess =
            timing_stats(&self.preprocess_us).expect("benchmark has preprocess samples");
        let forward = timing_stats(&self.forward_us).expect("benchmark has forward samples");
        let postprocess =
            timing_stats(&self.postprocess_us).expect("benchmark has postprocess samples");
        let total = timing_stats(&self.total_us).expect("benchmark has total samples");
        format!(
            "AKARS_TENNIS_BENCH_RESULT measured_runs={measured_runs} images={images} samples={} \
             decode_us_avg={} decode_us_p50={} decode_us_p95={} resize_us_avg={} resize_us_p50={} \
             resize_us_p95={} preprocess_us_avg={} preprocess_us_p50={} preprocess_us_p95={} \
             forward_us_avg={} forward_us_p50={} forward_us_p95={} postprocess_us_avg={} \
             postprocess_us_p50={} postprocess_us_p95={} total_us_avg={} total_us_p50={} \
             total_us_p95={}",
            self.total_us.len(),
            decode.average_us,
            decode.p50_us,
            decode.p95_us,
            resize.average_us,
            resize.p50_us,
            resize.p95_us,
            preprocess.average_us,
            preprocess.p50_us,
            preprocess.p95_us,
            forward.average_us,
            forward.p50_us,
            forward.p95_us,
            postprocess.average_us,
            postprocess.p50_us,
            postprocess.p95_us,
            total.average_us,
            total.p50_us,
            total.p95_us,
        )
    }
}

#[derive(Debug)]
struct ValidationError(String);

impl ValidationError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ValidationError {}

fn timing_stats(values: &[i64]) -> Option<TimingStats> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let sum: i128 = sorted.iter().map(|value| i128::from(*value)).sum();
    let average_us = (sum / sorted.len() as i128) as i64;
    Some(TimingStats {
        average_us,
        p50_us: nearest_rank(&sorted, 50),
        p95_us: nearest_rank(&sorted, 95),
    })
}

fn nearest_rank(sorted: &[i64], percentile: usize) -> i64 {
    debug_assert!(!sorted.is_empty());
    debug_assert!((1..=100).contains(&percentile));
    let rank = (percentile * sorted.len()).div_ceil(100);
    sorted[rank - 1]
}

fn main() {
    if let Err(err) = run() {
        println!(
            "AKARS_TENNIS_VALIDATE_FAIL reason={}",
            shell_word(&err.to_string())
        );
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = parse_cli(env::args().skip(1))?;
    let image_paths = read_image_list(&cli.image_list)?;
    if image_paths.is_empty() {
        return Err(Box::new(ValidationError::new("image list is empty")));
    }

    let cwd = env::current_dir()?;
    let list_root = cli.image_list.parent().unwrap_or_else(|| Path::new("."));
    let mut input_images = Vec::with_capacity(image_paths.len());
    for rel_path in image_paths {
        let image_path = resolve_image_path(&cwd, list_root, &rel_path);
        input_images.push(InputImage {
            path: rel_path,
            frame: CameraFrame {
                jpeg: fs::read(image_path)?,
                width: 0,
                height: 0,
            },
        });
    }

    let mut model = open_model(&cli.model)?;
    let config = InferenceConfig {
        classes_num: cli.classes,
        confidence_threshold: cli.conf,
        iou_threshold: cli.iou,
    };

    for _ in 0..cli.warmup {
        for input in &input_images {
            let _ = infer_frame(&mut model, &input.frame, config)?;
        }
    }
    if cli.warmup > 0 {
        println!(
            "AKARS_TENNIS_WARMUP_DONE runs={} images={}",
            cli.warmup,
            input_images.len()
        );
    }

    if cli.write_expected {
        let actual_set = run_measured_pass(&mut model, &input_images, config, 1, None)?;
        fs::write(
            &cli.expected,
            format_expected(&actual_set, cli.classes, cli.conf, cli.iou),
        )?;
        println!(
            "AKARS_TENNIS_EXPECTED_WRITTEN path={} images={}",
            cli.expected.display(),
            actual_set.images.len()
        );
        return Ok(());
    }

    let expected = parse_expected(&fs::read_to_string(&cli.expected)?)?;
    let mut samples = BenchmarkSamples::default();
    for run_index in 1..=cli.repeat {
        let actual_set = run_measured_pass(
            &mut model,
            &input_images,
            config,
            run_index,
            Some(&mut samples),
        )?;
        compare_expected(&expected, &actual_set)?;
    }
    println!("AKARS_TENNIS_VALIDATE_PASS images={}", input_images.len());
    println!("{}", samples.format_result(cli.repeat, input_images.len()));
    Ok(())
}

fn run_measured_pass(
    model: &mut tpu::YoloModel,
    input_images: &[InputImage],
    config: InferenceConfig,
    run_index: usize,
    mut samples: Option<&mut BenchmarkSamples>,
) -> Result<ExpectedSet, Box<dyn std::error::Error>> {
    let mut actual = Vec::with_capacity(input_images.len());
    for (index, input) in input_images.iter().enumerate() {
        let (normalized, timing, total_us) = infer_frame(model, &input.frame, config)?;

        println!(
            "AKARS_TENNIS_RESULT image={index} path={} detections={} run={run_index}",
            shell_word(&input.path),
            normalized.len()
        );
        for det in &normalized {
            println!(
                "AKARS_TENNIS_DET image={index} cls={} class={} score_q10000={} \
                 confidence_percent={:.2} left={} top={} right={} bottom={} run={run_index}",
                det.cls,
                class_name(det.cls),
                det.score_q10000,
                f64::from(det.score_q10000) / 100.0,
                det.left,
                det.top,
                det.right,
                det.bottom
            );
        }
        println!(
            "AKARS_TENNIS_TIMING image={index} preprocess_us={} forward_us={} postprocess_us={} \
             total_us={} run={run_index} decode_us={} resize_us={}",
            timing.preprocess_us,
            timing.forward_us,
            timing.postprocess_us,
            total_us,
            timing.decode_us,
            timing.resize_us
        );

        if let Some(samples) = samples.as_deref_mut() {
            samples.push(timing, total_us);
        }
        actual.push(ImageExpected {
            index,
            path: input.path.clone(),
            detections: normalized,
        });
    }

    Ok(ExpectedSet {
        images: actual,
        bbox_tolerance_px: DEFAULT_BBOX_TOLERANCE_PX,
        score_tolerance_q10000: DEFAULT_SCORE_TOLERANCE_Q10000,
    })
}

fn infer_frame(
    model: &mut tpu::YoloModel,
    frame: &CameraFrame,
    config: InferenceConfig,
) -> Result<(Vec<ExpectedDetection>, InferTiming, i64), tpu::TpuError> {
    let mut timing = InferTiming::default();
    let total_start = Instant::now();
    let detections = model.infer_timed(frame, config, Some(&mut timing))?;
    let total_us = total_start.elapsed().as_micros() as i64;
    Ok((normalize_detections(&detections), timing, total_us))
}

fn parse_cli(args: impl Iterator<Item = String>) -> Result<Cli, ValidationError> {
    let mut cli = Cli {
        model: PathBuf::new(),
        image_list: PathBuf::new(),
        expected: PathBuf::new(),
        classes: 1,
        conf: 0.5,
        iou: 0.5,
        write_expected: false,
        warmup: 0,
        repeat: 1,
    };
    let mut positional = 0;
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            "--classes" => cli.classes = take_parse(&mut args, "--classes")?,
            "--conf" => cli.conf = take_parse(&mut args, "--conf")?,
            "--iou" => cli.iou = take_parse(&mut args, "--iou")?,
            "--warmup" => cli.warmup = take_parse(&mut args, "--warmup")?,
            "--repeat" => cli.repeat = take_parse(&mut args, "--repeat")?,
            "--write-expected" => cli.write_expected = true,
            value if value.starts_with('-') => {
                return Err(ValidationError::new(format!("unknown option: {value}")));
            }
            value => {
                match positional {
                    0 => cli.model = PathBuf::from(value),
                    1 => cli.image_list = PathBuf::from(value),
                    2 => cli.expected = PathBuf::from(value),
                    _ => {
                        return Err(ValidationError::new(format!(
                            "unexpected argument: {value}"
                        )));
                    }
                }
                positional += 1;
            }
        }
    }

    if cli.model.as_os_str().is_empty() {
        return Err(ValidationError::new("missing model path"));
    }
    if cli.image_list.as_os_str().is_empty() {
        return Err(ValidationError::new("missing image list path"));
    }
    if cli.expected.as_os_str().is_empty() {
        return Err(ValidationError::new("missing expected path"));
    }
    if cli.classes <= 0 {
        return Err(ValidationError::new("--classes must be positive"));
    }
    if cli.repeat == 0 {
        return Err(ValidationError::new("--repeat must be positive"));
    }
    if cli.write_expected && (cli.warmup != 0 || cli.repeat != 1) {
        return Err(ValidationError::new(
            "--write-expected requires --warmup 0 and --repeat 1",
        ));
    }
    Ok(cli)
}

fn take_parse<T: std::str::FromStr>(
    args: &mut std::iter::Peekable<impl Iterator<Item = String>>,
    option: &str,
) -> Result<T, ValidationError> {
    let value = args
        .next()
        .ok_or_else(|| ValidationError::new(format!("{option} expects a value")))?;
    value
        .parse()
        .map_err(|_| ValidationError::new(format!("{option} has invalid value: {value}")))
}

fn read_image_list(path: &Path) -> Result<Vec<String>, std::io::Error> {
    let text = fs::read_to_string(path)?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(ToOwned::to_owned)
        .collect())
}

fn resolve_image_path(cwd: &Path, list_root: &Path, entry: &str) -> PathBuf {
    let entry_path = Path::new(entry);
    if entry_path.is_absolute() {
        return entry_path.to_path_buf();
    }

    let cwd_relative = cwd.join(entry_path);
    if cwd_relative.exists() {
        return cwd_relative;
    }

    list_root.join(entry_path)
}

fn normalize_detections(detections: &[Detection]) -> Vec<ExpectedDetection> {
    let mut normalized: Vec<_> = detections.iter().map(detection_to_expected).collect();
    normalized.sort_by(|a, b| {
        a.cls
            .cmp(&b.cls)
            .then_with(|| b.score_q10000.cmp(&a.score_q10000))
            .then_with(|| a.left.cmp(&b.left))
            .then_with(|| a.top.cmp(&b.top))
    });
    normalized
}

fn detection_to_expected(d: &Detection) -> ExpectedDetection {
    let left = (d.bbox.x - d.bbox.w * 0.5).round() as i32;
    let top = (d.bbox.y - d.bbox.h * 0.5).round() as i32;
    let right = (d.bbox.x + d.bbox.w * 0.5).round() as i32;
    let bottom = (d.bbox.y + d.bbox.h * 0.5).round() as i32;
    ExpectedDetection {
        cls: d.cls,
        score_q10000: (d.score * 10000.0).round() as i32,
        left,
        top,
        right,
        bottom,
    }
}

fn format_expected(expected: &ExpectedSet, classes: i32, conf: f32, iou: f32) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "AKARS_VALIDATE_EXPECTED version=1 image_count={} classes={} conf_q10000={} iou_q10000={} \
         bbox_tolerance_px={} score_tolerance_q10000={}\n",
        expected.images.len(),
        classes,
        (conf * 10000.0).round() as i32,
        (iou * 10000.0).round() as i32,
        expected.bbox_tolerance_px,
        expected.score_tolerance_q10000
    ));
    for image in &expected.images {
        out.push_str(&format!(
            "image index={} path={} count={}\n",
            image.index,
            image.path,
            image.detections.len()
        ));
        for det in &image.detections {
            out.push_str(&format!(
                "det image={} cls={} score_q10000={} left={} top={} right={} bottom={}\n",
                image.index, det.cls, det.score_q10000, det.left, det.top, det.right, det.bottom
            ));
        }
    }
    out
}

fn parse_expected(text: &str) -> Result<ExpectedSet, ValidationError> {
    let mut lines = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'));
    let header = lines
        .next()
        .ok_or_else(|| ValidationError::new("expected file is empty"))?;
    if !header.starts_with("AKARS_VALIDATE_EXPECTED ") {
        return Err(ValidationError::new("invalid expected header"));
    }
    let header_fields = parse_fields(header);
    let version = parse_field::<i32>(&header_fields, "version")?;
    if version != 1 {
        return Err(ValidationError::new(format!(
            "unsupported expected version: {version}"
        )));
    }
    let image_count = parse_field::<usize>(&header_fields, "image_count")?;
    let bbox_tolerance_px = parse_field_or(
        &header_fields,
        "bbox_tolerance_px",
        DEFAULT_BBOX_TOLERANCE_PX,
    )?;
    let score_tolerance_q10000 = parse_field_or(
        &header_fields,
        "score_tolerance_q10000",
        DEFAULT_SCORE_TOLERANCE_Q10000,
    )?;

    let mut images = Vec::new();
    let mut current: Option<ImageExpected> = None;

    for line in lines {
        let fields = parse_fields(line);
        if line.starts_with("image ") {
            if let Some(image) = current.take() {
                images.push(image);
            }
            current = Some(ImageExpected {
                index: parse_field(&fields, "index")?,
                path: parse_field::<String>(&fields, "path")?,
                detections: Vec::with_capacity(parse_field_or(&fields, "count", 0usize)?),
            });
        } else if line.starts_with("det ") {
            let image_index: usize = parse_field(&fields, "image")?;
            let image = current
                .as_mut()
                .ok_or_else(|| ValidationError::new("det line appears before image line"))?;
            if image.index != image_index {
                return Err(ValidationError::new(format!(
                    "det image index mismatch: got {image_index}, current {}",
                    image.index
                )));
            }
            image.detections.push(ExpectedDetection {
                cls: parse_field(&fields, "cls")?,
                score_q10000: parse_field(&fields, "score_q10000")?,
                left: parse_field(&fields, "left")?,
                top: parse_field(&fields, "top")?,
                right: parse_field(&fields, "right")?,
                bottom: parse_field(&fields, "bottom")?,
            });
        } else {
            return Err(ValidationError::new(format!(
                "unknown expected line: {line}"
            )));
        }
    }
    if let Some(image) = current {
        images.push(image);
    }
    if images.len() != image_count {
        return Err(ValidationError::new(format!(
            "expected image count mismatch: header={image_count} actual={}",
            images.len()
        )));
    }
    Ok(ExpectedSet {
        images,
        bbox_tolerance_px,
        score_tolerance_q10000,
    })
}

fn compare_expected(expected: &ExpectedSet, actual: &ExpectedSet) -> Result<(), ValidationError> {
    if expected.images.len() != actual.images.len() {
        return Err(ValidationError::new(format!(
            "image count mismatch expected={} actual={}",
            expected.images.len(),
            actual.images.len()
        )));
    }

    for (exp_image, act_image) in expected.images.iter().zip(&actual.images) {
        if exp_image.index != act_image.index || exp_image.path != act_image.path {
            return Err(ValidationError::new(format!(
                "image metadata mismatch expected=index:{} path:{} actual=index:{} path:{}",
                exp_image.index, exp_image.path, act_image.index, act_image.path
            )));
        }
        if exp_image.detections.len() != act_image.detections.len() {
            return Err(ValidationError::new(format!(
                "detection count mismatch image={} expected={} actual={}",
                exp_image.index,
                exp_image.detections.len(),
                act_image.detections.len()
            )));
        }
        for (det_index, (exp, act)) in exp_image
            .detections
            .iter()
            .zip(&act_image.detections)
            .enumerate()
        {
            if exp.cls != act.cls {
                return Err(ValidationError::new(format!(
                    "class mismatch image={} det={} expected={} actual={}",
                    exp_image.index, det_index, exp.cls, act.cls
                )));
            }
            let score_delta = (exp.score_q10000 - act.score_q10000).abs();
            if score_delta > expected.score_tolerance_q10000 {
                return Err(ValidationError::new(format!(
                    "score mismatch image={} det={} expected={} actual={} tolerance={}",
                    exp_image.index,
                    det_index,
                    exp.score_q10000,
                    act.score_q10000,
                    expected.score_tolerance_q10000
                )));
            }
            for (name, exp_value, act_value) in [
                ("left", exp.left, act.left),
                ("top", exp.top, act.top),
                ("right", exp.right, act.right),
                ("bottom", exp.bottom, act.bottom),
            ] {
                if (exp_value - act_value).abs() > expected.bbox_tolerance_px {
                    return Err(ValidationError::new(format!(
                        "bbox mismatch image={} det={} field={} expected={} actual={} tolerance={}",
                        exp_image.index,
                        det_index,
                        name,
                        exp_value,
                        act_value,
                        expected.bbox_tolerance_px
                    )));
                }
            }
        }
    }
    Ok(())
}

fn parse_fields(line: &str) -> Vec<(&str, &str)> {
    line.split_whitespace()
        .filter_map(|field| field.split_once('='))
        .collect()
}

fn field_value<'a>(fields: &'a [(&str, &str)], key: &str) -> Option<&'a str> {
    fields
        .iter()
        .find_map(|(field_key, value)| (*field_key == key).then_some(*value))
}

fn parse_field<T: std::str::FromStr>(
    fields: &[(&str, &str)],
    key: &str,
) -> Result<T, ValidationError> {
    let value = field_value(fields, key)
        .ok_or_else(|| ValidationError::new(format!("missing field: {key}")))?;
    value
        .parse()
        .map_err(|_| ValidationError::new(format!("invalid field {key}: {value}")))
}

fn parse_field_or<T: std::str::FromStr>(
    fields: &[(&str, &str)],
    key: &str,
    default: T,
) -> Result<T, ValidationError> {
    match field_value(fields, key) {
        Some(value) => value
            .parse()
            .map_err(|_| ValidationError::new(format!("invalid field {key}: {value}"))),
        None => Ok(default),
    }
}

fn shell_word(value: &str) -> String {
    value.replace(' ', "_")
}

fn class_name(cls: i32) -> &'static str {
    match cls {
        0 => "tennis_ball",
        _ => "unknown",
    }
}

fn print_usage() {
    eprintln!(
        "Usage: akars-tennis-validator <model.cvimodel> <images.txt> <expected.txt> \
         [--write-expected] [--classes N] [--conf X] [--iou X] [--warmup N] [--repeat N]"
    );
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn cli_args(extra: &[&str]) -> Vec<String> {
        ["model.cvimodel", "images.txt", "expected.txt"]
            .into_iter()
            .chain(extra.iter().copied())
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn benchmark_cli_defaults_to_one_measured_pass() {
        let cli = parse_cli(cli_args(&[]).into_iter()).unwrap();
        assert_eq!(cli.warmup, 0);
        assert_eq!(cli.repeat, 1);
    }

    #[test]
    fn benchmark_cli_accepts_warmup_and_repeat() {
        let cli = parse_cli(cli_args(&["--warmup", "2", "--repeat", "5"]).into_iter()).unwrap();
        assert_eq!(cli.warmup, 2);
        assert_eq!(cli.repeat, 5);
    }

    #[test]
    fn benchmark_cli_rejects_zero_repeat() {
        let error = parse_cli(cli_args(&["--repeat", "0"]).into_iter()).unwrap_err();
        assert!(error.to_string().contains("--repeat must be positive"));
    }

    #[test]
    fn write_expected_rejects_non_default_benchmark_parameters() {
        let error = parse_cli(
            cli_args(&["--write-expected", "--warmup", "1", "--repeat", "2"]).into_iter(),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("requires --warmup 0 and --repeat 1")
        );
    }

    #[test]
    fn timing_stats_use_nearest_rank_percentiles() {
        assert_eq!(
            timing_stats(&[1, 2, 3, 4, 100]),
            Some(TimingStats {
                average_us: 22,
                p50_us: 3,
                p95_us: 100,
            })
        );
        assert_eq!(timing_stats(&[]), None);
    }

    #[test]
    fn benchmark_summary_has_stable_stage_fields() {
        let mut samples = BenchmarkSamples::default();
        samples.push(
            InferTiming {
                decode_us: 10,
                resize_us: 20,
                preprocess_us: 30,
                forward_us: 40,
                postprocess_us: 50,
            },
            120,
        );
        samples.push(
            InferTiming {
                decode_us: 20,
                resize_us: 30,
                preprocess_us: 50,
                forward_us: 60,
                postprocess_us: 70,
            },
            180,
        );

        assert_eq!(
            samples.format_result(1, 2),
            "AKARS_TENNIS_BENCH_RESULT measured_runs=1 images=2 samples=2 decode_us_avg=15 \
             decode_us_p50=10 decode_us_p95=20 resize_us_avg=25 resize_us_p50=20 resize_us_p95=30 \
             preprocess_us_avg=40 preprocess_us_p50=30 preprocess_us_p95=50 forward_us_avg=50 \
             forward_us_p50=40 forward_us_p95=60 postprocess_us_avg=60 postprocess_us_p50=50 \
             postprocess_us_p95=70 total_us_avg=150 total_us_p50=120 total_us_p95=180"
        );
    }

    #[test]
    fn resolves_paths_relative_to_cwd_first() {
        let tmp = unique_temp_dir("cwd-first");
        let cwd = tmp.join("case");
        let list_root = cwd.join("validation");
        let image = cwd.join("validation/tennis.jpg");
        fs::create_dir_all(&list_root).unwrap();
        fs::write(&image, b"jpeg").unwrap();

        assert_eq!(
            resolve_image_path(&cwd, &list_root, "validation/tennis.jpg"),
            image
        );

        fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn falls_back_to_list_root_for_plain_entries() {
        let tmp = unique_temp_dir("list-root");
        let cwd = tmp.join("case");
        let list_root = cwd.join("validation");
        let image = list_root.join("tennis.jpg");
        fs::create_dir_all(&list_root).unwrap();
        fs::write(&image, b"jpeg").unwrap();

        assert_eq!(resolve_image_path(&cwd, &list_root, "tennis.jpg"), image);

        fs::remove_dir_all(tmp).unwrap();
    }

    fn unique_temp_dir(name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!(
            "akars-tennis-validator-{name}-{}-{suffix}",
            std::process::id()
        ))
    }
}
