#[derive(Clone, Copy, Debug, Default)]
pub struct Box2d {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Detection {
    pub bbox: Box2d,
    pub cls: i32,
    pub score: f32,
    pub batch_idx: i32,
}

pub fn cal_iou(a: Box2d, b: Box2d) -> f32 {
    let area1 = a.w * a.h;
    let area2 = b.w * b.h;
    let wi = (a.x + a.w / 2.0).min(b.x + b.w / 2.0) - (a.x - a.w / 2.0).max(b.x - b.w / 2.0);
    let hi = (a.y + a.h / 2.0).min(b.y + b.h / 2.0) - (a.y - a.h / 2.0).max(b.y - b.h / 2.0);
    let intersection = wi.max(0.0) * hi.max(0.0);
    let denom = area1 + area2 - intersection;
    if denom <= 0.0 {
        0.0
    } else {
        intersection / denom
    }
}

pub fn nms(detections: &mut Vec<Detection>, threshold: f32) {
    detections.sort_by(|a, b| b.score.total_cmp(&a.score));

    for i in 0..detections.len() {
        if detections[i].score == 0.0 {
            continue;
        }
        for j in (i + 1)..detections.len() {
            if detections[j].score != 0.0
                && detections[i].batch_idx == detections[j].batch_idx
                && detections[i].cls == detections[j].cls
                && cal_iou(detections[i].bbox, detections[j].bbox) > threshold
            {
                detections[j].score = 0.0;
            }
        }
    }

    detections.retain(|detection| detection.score != 0.0);
}

pub fn correct_yolo_boxes(
    detections: &mut [Detection],
    image_h: i32,
    image_w: i32,
    input_h: i32,
    input_w: i32,
) {
    let scale = (input_w as f32 / image_w as f32).min(input_h as f32 / image_h as f32);
    let new_h = (image_h as f32 * scale) as i32;
    let new_w = (image_w as f32 * scale) as i32;
    let pad_top = (input_h - new_h) / 2;
    let pad_left = (input_w - new_w) / 2;

    for detection in detections {
        let cx = detection.bbox.x;
        let cy = detection.bbox.y;
        let w = detection.bbox.w;
        let h = detection.bbox.h;

        let mut x1 = cx - 0.5 * w;
        let mut y1 = cy - 0.5 * h;
        let mut x2 = cx + 0.5 * w;
        let mut y2 = cy + 0.5 * h;

        x1 = ((x1 - pad_left as f32) / scale).max(0.0);
        y1 = ((y1 - pad_top as f32) / scale).max(0.0);
        x2 = ((x2 - pad_left as f32) / scale).min(image_w as f32);
        y2 = ((y2 - pad_top as f32) / scale).min(image_h as f32);

        detection.bbox.x = (x1 + x2) / 2.0;
        detection.bbox.y = (y1 + y2) / 2.0;
        detection.bbox.w = x2 - x1;
        detection.bbox.h = y2 - y1;
    }
}

pub fn parse_yolov8_output(
    data: &[f32],
    shape: [i32; 4],
    classes_num: i32,
    confidence_threshold: f32,
) -> Vec<Detection> {
    let batch = shape[0] as usize;
    let channels = shape[1] as usize;
    let num_boxes = shape[2] as usize;
    let classes_num = classes_num as usize;
    let mut detections = Vec::new();

    for b in 0..batch {
        let batch_base = b * channels * num_boxes;
        let cx_row = batch_base;
        let cy_row = batch_base + num_boxes;
        let w_row = batch_base + 2 * num_boxes;
        let h_row = batch_base + 3 * num_boxes;

        for j in 0..num_boxes {
            let mut max_score = -1.0f32;
            let mut max_cls = 0usize;
            for c in 0..classes_num {
                let score = data[batch_base + (4 + c) * num_boxes + j];
                if score > max_score {
                    max_score = score;
                    max_cls = c;
                }
            }

            if max_score <= confidence_threshold {
                continue;
            }

            detections.push(Detection {
                bbox: Box2d {
                    x: data[cx_row + j],
                    y: data[cy_row + j],
                    w: data[w_row + j],
                    h: data[h_row + j],
                },
                cls: max_cls as i32,
                score: max_score,
                batch_idx: b as i32,
            });
        }
    }

    detections
}

#[cfg(test)]
mod tests {
    use super::{Box2d, Detection, cal_iou, correct_yolo_boxes, nms, parse_yolov8_output};

    #[test]
    fn iou_identical_boxes_is_one() {
        let b = Box2d {
            x: 10.0,
            y: 10.0,
            w: 4.0,
            h: 4.0,
        };
        assert!((cal_iou(b, b) - 1.0).abs() < 0.0001);
    }

    #[test]
    fn nms_removes_lower_duplicate() {
        let mut detections = vec![
            Detection {
                bbox: Box2d {
                    x: 10.0,
                    y: 10.0,
                    w: 4.0,
                    h: 4.0,
                },
                cls: 0,
                score: 0.9,
                batch_idx: 0,
            },
            Detection {
                bbox: Box2d {
                    x: 10.2,
                    y: 10.2,
                    w: 4.0,
                    h: 4.0,
                },
                cls: 0,
                score: 0.8,
                batch_idx: 0,
            },
        ];
        nms(&mut detections, 0.5);
        assert_eq!(detections.len(), 1);
        assert_eq!(detections[0].score, 0.9);
    }

    #[test]
    fn parses_yolov8_channel_first_output() {
        let data = [
            10.0, 20.0, // cx
            11.0, 21.0, // cy
            4.0, 5.0, // w
            6.0, 7.0, // h
            0.4, 0.9, // class 0
        ];
        let detections = parse_yolov8_output(&data, [1, 5, 2, 1], 1, 0.5);
        assert_eq!(detections.len(), 1);
        assert_eq!(detections[0].bbox.x, 20.0);
        assert_eq!(detections[0].score, 0.9);
    }

    #[test]
    fn corrects_letterbox_coordinates() {
        let mut detections = vec![Detection {
            bbox: Box2d {
                x: 320.0,
                y: 320.0,
                w: 100.0,
                h: 100.0,
            },
            cls: 0,
            score: 1.0,
            batch_idx: 0,
        }];
        correct_yolo_boxes(&mut detections, 480, 640, 640, 640);
        assert!((detections[0].bbox.x - 320.0).abs() < 0.01);
        assert!((detections[0].bbox.y - 240.0).abs() < 0.01);
    }
}
