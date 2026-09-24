use std::{error::Error, fmt, path::Path};

use crate::{camera::CameraFrame, detector::Detection};

#[derive(Clone, Copy, Debug)]
pub struct InferenceConfig {
    pub classes_num: i32,
    pub confidence_threshold: f32,
    pub iou_threshold: f32,
}

impl Default for InferenceConfig {
    fn default() -> Self {
        Self {
            classes_num: 1,
            confidence_threshold: 0.5,
            iou_threshold: 0.5,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct InferTiming {
    /// JPEG decode microseconds.
    pub decode_us: i64,
    /// Resize, letterbox clear, and planar pack microseconds.
    pub resize_us: i64,
    /// Decode plus CPU-side resize, letterbox, and tensor packing microseconds.
    pub preprocess_us: i64,
    /// CVI_NN_Forward microseconds.
    pub forward_us: i64,
    /// detection parse + dequant + NMS + box correction microseconds.
    pub postprocess_us: i64,
}

#[derive(Debug)]
pub struct TpuError(String);

impl TpuError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for TpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for TpuError {}

#[cfg(all(target_arch = "riscv64", not(akars_no_tpu)))]
mod imp {
    use std::{
        ffi::{CString, c_char, c_int, c_void},
        os::unix::ffi::OsStrExt,
        path::Path,
        ptr, slice,
        time::Instant,
    };

    use super::{CameraFrame, Detection, InferTiming, InferenceConfig, TpuError};
    use crate::{
        detector::{correct_yolo_boxes, nms, parse_yolov8_output},
        image_bridge::{self, ImagePreprocessTiming},
        yuv420::{PlanarYuv420, Yuv420Preprocessor},
    };

    const CVI_FMT_FP32: i32 = 0;
    const CVI_FMT_BF16: i32 = 3;
    const CVI_FMT_INT16: i32 = 4;
    const CVI_FMT_INT8: i32 = 6;
    const CVI_FMT_UINT8: i32 = 7;
    const CVI_RC_SUCCESS: i32 = 0;
    const CVI_DIM_MAX: usize = 6;

    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    struct CviShape {
        dim: [i32; CVI_DIM_MAX],
        dim_size: usize,
    }

    #[repr(C)]
    #[derive(Debug)]
    struct CviTensor {
        name: *mut c_char,
        shape: CviShape,
        fmt: i32,
        count: usize,
        mem_size: usize,
        sys_mem: *mut u8,
        paddr: u64,
        mem_type: i32,
        qscale: f32,
        zero_point: c_int,
        pixel_format: i32,
        aligned: bool,
        mean: [f32; 3],
        scale: [f32; 3],
        owner: *mut c_void,
        reserved: [c_char; 32],
    }

    type CviModelHandle = *mut c_void;

    unsafe extern "C" {
        fn CVI_NN_RegisterModel(model_file: *const c_char, model: *mut CviModelHandle) -> i32;
        fn CVI_NN_GetInputOutputTensors(
            model: CviModelHandle,
            inputs: *mut *mut CviTensor,
            input_num: *mut i32,
            outputs: *mut *mut CviTensor,
            output_num: *mut i32,
        ) -> i32;
        fn CVI_NN_GetTensorByName(
            name: *const c_char,
            tensors: *mut CviTensor,
            num: i32,
        ) -> *mut CviTensor;
        fn CVI_NN_TensorPtr(tensor: *mut CviTensor) -> *mut c_void;
        fn CVI_NN_TensorShape(tensor: *mut CviTensor) -> CviShape;
        fn CVI_NN_Forward(
            model: CviModelHandle,
            inputs: *mut CviTensor,
            input_num: i32,
            outputs: *mut CviTensor,
            output_num: i32,
        ) -> i32;
        fn CVI_NN_CleanupModel(model: CviModelHandle) -> i32;
    }

    pub struct YoloModel {
        model: CviModelHandle,
        inputs: *mut CviTensor,
        input_num: i32,
        outputs: *mut CviTensor,
        output_num: i32,
        input: *mut CviTensor,
        input_h: i32,
        input_w: i32,
        output_shapes: Vec<CviShape>,
        preprocessor: image_bridge::ImagePreprocessor,
        yuv_preprocessor: Yuv420Preprocessor,
    }

    impl YoloModel {
        pub fn open(path: &Path) -> Result<Self, TpuError> {
            let c_path = CString::new(path.as_os_str().as_bytes())
                .map_err(|_| TpuError::new("model path contains NUL byte"))?;
            let mut model: CviModelHandle = ptr::null_mut();
            let rc = unsafe { CVI_NN_RegisterModel(c_path.as_ptr(), &mut model) };
            if rc != CVI_RC_SUCCESS {
                return Err(TpuError::new(format!("CVI_NN_RegisterModel failed: {rc}")));
            }

            let mut inputs = ptr::null_mut();
            let mut outputs = ptr::null_mut();
            let mut input_num = 0;
            let mut output_num = 0;
            let rc = unsafe {
                CVI_NN_GetInputOutputTensors(
                    model,
                    &mut inputs,
                    &mut input_num,
                    &mut outputs,
                    &mut output_num,
                )
            };
            if rc != CVI_RC_SUCCESS {
                unsafe {
                    CVI_NN_CleanupModel(model);
                }
                return Err(TpuError::new(format!(
                    "CVI_NN_GetInputOutputTensors failed: {rc}"
                )));
            }

            let input = unsafe { CVI_NN_GetTensorByName(ptr::null(), inputs, input_num) };
            if input.is_null() {
                unsafe {
                    CVI_NN_CleanupModel(model);
                }
                return Err(TpuError::new("default input tensor not found"));
            }

            let input_shape = unsafe { CVI_NN_TensorShape(input) };
            let input_h = input_shape.dim[2];
            let input_w = input_shape.dim[3];
            let outputs_slice = unsafe { slice::from_raw_parts_mut(outputs, output_num as usize) };
            let output_shapes = outputs_slice
                .iter_mut()
                .map(|tensor| unsafe { CVI_NN_TensorShape(tensor as *mut CviTensor) })
                .collect();

            Ok(Self {
                model,
                inputs,
                input_num,
                outputs,
                output_num,
                input,
                input_h,
                input_w,
                output_shapes,
                preprocessor: image_bridge::ImagePreprocessor::new(),
                yuv_preprocessor: Yuv420Preprocessor::new(),
            })
        }

        pub fn infer(
            &mut self,
            frame: &CameraFrame,
            config: InferenceConfig,
        ) -> Result<Vec<Detection>, TpuError> {
            self.infer_timed(frame, config, None)
        }

        pub fn infer_timed(
            &mut self,
            frame: &CameraFrame,
            config: InferenceConfig,
            timing: Option<&mut InferTiming>,
        ) -> Result<Vec<Detection>, TpuError> {
            let (input_ptr, input_len) = self.input_buffer()?;
            let input = unsafe { slice::from_raw_parts_mut(input_ptr, input_len) };

            let pre_start = Instant::now();
            let preprocess = self
                .preprocessor
                .mjpeg_to_rgb_planar(&frame.jpeg, input, self.input_w, self.input_h)
                .map_err(|err| TpuError::new(format!("MJPEG decode/preprocess failed: {err}")))?;
            let preprocess_us = pre_start.elapsed().as_micros() as i64;

            self.forward_preprocessed(
                preprocess,
                preprocess_us,
                frame.width,
                frame.height,
                config,
                timing,
            )
        }

        pub fn infer_yuv420_timed(
            &mut self,
            frame: PlanarYuv420<'_>,
            decode_us: i64,
            config: InferenceConfig,
            timing: Option<&mut InferTiming>,
        ) -> Result<Vec<Detection>, TpuError> {
            let (input_ptr, input_len) = self.input_buffer()?;
            let input = unsafe { slice::from_raw_parts_mut(input_ptr, input_len) };
            let layout = frame.layout();
            let pre_start = Instant::now();
            let preprocess = self
                .yuv_preprocessor
                .preprocess_into(frame, input, self.input_w, self.input_h, decode_us)
                .map_err(|err| TpuError::new(format!("JPU YUV420 preprocess failed: {err}")))?;
            let preprocess_us = decode_us
                .saturating_add(i64::try_from(pre_start.elapsed().as_micros()).unwrap_or(i64::MAX));

            self.forward_preprocessed(
                preprocess,
                preprocess_us,
                layout.source.width,
                layout.source.height,
                config,
                timing,
            )
        }

        fn input_buffer(&self) -> Result<(*mut u8, usize), TpuError> {
            let input_ptr = unsafe { CVI_NN_TensorPtr(self.input) as *mut u8 };
            if input_ptr.is_null() {
                return Err(TpuError::new("input tensor pointer is null"));
            }
            let input_len = rgb_tensor_len(self.input_w, self.input_h)?;
            let input_tensor = unsafe { &*self.input };
            if input_tensor.mem_size < input_len {
                return Err(TpuError::new(format!(
                    "input tensor buffer is too small: mem_size={} required={input_len}",
                    input_tensor.mem_size
                )));
            }
            Ok((input_ptr, input_len))
        }

        fn forward_preprocessed(
            &mut self,
            preprocess: ImagePreprocessTiming,
            preprocess_us: i64,
            fallback_width: u32,
            fallback_height: u32,
            config: InferenceConfig,
            timing: Option<&mut InferTiming>,
        ) -> Result<Vec<Detection>, TpuError> {
            let fwd_start = Instant::now();
            let rc = unsafe {
                CVI_NN_Forward(
                    self.model,
                    self.inputs,
                    self.input_num,
                    self.outputs,
                    self.output_num,
                )
            };
            let forward_us = fwd_start.elapsed().as_micros() as i64;
            if rc != CVI_RC_SUCCESS {
                return Err(TpuError::new(format!("CVI_NN_Forward failed: {rc}")));
            }

            let post_start = Instant::now();
            let mut detections = self.get_detections(config)?;
            nms(&mut detections, config.iou_threshold);

            let image_w = if preprocess.src_w > 0 {
                preprocess.src_w
            } else {
                fallback_width as i32
            };
            let image_h = if preprocess.src_h > 0 {
                preprocess.src_h
            } else {
                fallback_height as i32
            };
            correct_yolo_boxes(
                &mut detections,
                image_h,
                image_w,
                self.input_h,
                self.input_w,
            );
            let postprocess_us = post_start.elapsed().as_micros() as i64;

            if let Some(t) = timing {
                *t = InferTiming {
                    decode_us: preprocess.decode_us,
                    resize_us: preprocess.resize_us,
                    preprocess_us,
                    forward_us,
                    postprocess_us,
                };
            }
            Ok(detections)
        }

        /// Run inference on a standalone image and write a copy with the
        /// detection boxes drawn to out_path.
        pub fn detect_image(
            &mut self,
            image: &[u8],
            out_path: &Path,
            config: InferenceConfig,
        ) -> Result<Vec<Detection>, TpuError> {
            let frame = CameraFrame {
                jpeg: image.to_vec(),
                width: 0,
                height: 0,
            };
            let detections = self.infer(&frame, config)?;

            image_bridge::draw_detections(image, &detections, out_path)
                .map_err(|err| TpuError::new(format!("failed to write annotated image: {err}")))?;
            Ok(detections)
        }

        fn get_detections(&mut self, config: InferenceConfig) -> Result<Vec<Detection>, TpuError> {
            if self.output_num < 1 || self.output_shapes.is_empty() {
                return Err(TpuError::new("model has no output tensor"));
            }
            let output = unsafe { &mut *self.outputs };
            let shape = self.output_shapes[0];
            let count = output.count;
            let ptr = unsafe { CVI_NN_TensorPtr(output as *mut CviTensor) };
            if ptr.is_null() {
                return Err(TpuError::new("output tensor pointer is null"));
            }

            let data = tensor_to_f32(output, ptr, count)?;
            Ok(parse_yolov8_output(
                &data,
                [shape.dim[0], shape.dim[1], shape.dim[2], shape.dim[3]],
                config.classes_num,
                config.confidence_threshold,
            ))
        }
    }

    impl Drop for YoloModel {
        fn drop(&mut self) {
            if !self.model.is_null() {
                unsafe {
                    CVI_NN_CleanupModel(self.model);
                }
            }
        }
    }

    fn rgb_tensor_len(width: i32, height: i32) -> Result<usize, TpuError> {
        if width <= 0 || height <= 0 {
            return Err(TpuError::new("input tensor dimensions must be positive"));
        }
        (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(3))
            .ok_or_else(|| TpuError::new("input tensor dimensions overflow"))
    }

    fn tensor_to_f32(
        tensor: &CviTensor,
        ptr: *mut c_void,
        count: usize,
    ) -> Result<Vec<f32>, TpuError> {
        match tensor.fmt {
            CVI_FMT_FP32 => {
                let src = unsafe { slice::from_raw_parts(ptr as *const f32, count) };
                Ok(src.to_vec())
            }
            CVI_FMT_INT8 => {
                let src = unsafe { slice::from_raw_parts(ptr as *const i8, count) };
                Ok(src.iter().map(|v| *v as f32 * tensor.qscale).collect())
            }
            CVI_FMT_UINT8 => {
                let src = unsafe { slice::from_raw_parts(ptr as *const u8, count) };
                Ok(src
                    .iter()
                    .map(|v| (*v as i32 - tensor.zero_point) as f32 * tensor.qscale)
                    .collect())
            }
            CVI_FMT_BF16 => {
                let src = unsafe { slice::from_raw_parts(ptr as *const u16, count) };
                Ok(src
                    .iter()
                    .map(|v| f32::from_bits((*v as u32) << 16))
                    .collect())
            }
            CVI_FMT_INT16 => {
                let src = unsafe { slice::from_raw_parts(ptr as *const i16, count) };
                Ok(src.iter().map(|v| *v as f32 * tensor.qscale).collect())
            }
            other => Err(TpuError::new(format!(
                "unsupported output tensor format: {other}"
            ))),
        }
    }
}

#[cfg(any(not(target_arch = "riscv64"), akars_no_tpu))]
mod imp {
    use std::path::Path;

    use super::{CameraFrame, Detection, InferTiming, InferenceConfig, TpuError};
    use crate::yuv420::PlanarYuv420;

    pub struct YoloModel;

    impl YoloModel {
        pub fn open(_path: &Path) -> Result<Self, TpuError> {
            Err(TpuError::new(
                "akars was built without SG2002 TPU runtime support",
            ))
        }

        pub fn infer(
            &mut self,
            _frame: &CameraFrame,
            _config: InferenceConfig,
        ) -> Result<Vec<Detection>, TpuError> {
            Err(TpuError::new(
                "akars was built without SG2002 TPU runtime support",
            ))
        }

        pub fn infer_timed(
            &mut self,
            _frame: &CameraFrame,
            _config: InferenceConfig,
            _timing: Option<&mut InferTiming>,
        ) -> Result<Vec<Detection>, TpuError> {
            Err(TpuError::new(
                "akars was built without SG2002 TPU runtime support",
            ))
        }

        pub fn infer_yuv420_timed(
            &mut self,
            _frame: PlanarYuv420<'_>,
            _decode_us: i64,
            _config: InferenceConfig,
            _timing: Option<&mut InferTiming>,
        ) -> Result<Vec<Detection>, TpuError> {
            Err(TpuError::new(
                "akars was built without SG2002 TPU runtime support",
            ))
        }

        pub fn detect_image(
            &mut self,
            _image: &[u8],
            _out_path: &Path,
            _config: InferenceConfig,
        ) -> Result<Vec<Detection>, TpuError> {
            Err(TpuError::new(
                "akars was built without SG2002 TPU runtime support",
            ))
        }
    }
}

pub use imp::YoloModel;

pub fn open_model(path: impl AsRef<Path>) -> Result<YoloModel, TpuError> {
    YoloModel::open(path.as_ref())
}
