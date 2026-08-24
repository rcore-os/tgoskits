#!/usr/bin/env python3
"""Run a reproducible YOLO11n fixture and emit bounded Task-3 decisions.

This is the Linux-side perception supplement for Task-3.  It deliberately
does not replace the current no_std CNN Guest controller: the ONNX runtime is
not a dependency of the AArch64 Guest image.  Instead, it verifies the same
YOLO detection -> bounded target contract that a future Guest/NPU adapter must
use, with model and fixture hashes recorded in the output manifest.

The default fixtures are the three checked-in tennis images used by the
StarryOS YOLO validation app.  They are useful here because they are stable,
small, and already part of this repository; the generic COCO model may detect
nearby objects rather than a tennis ball, so class semantics are reported and
never silently relabelled.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
import urllib.request
from pathlib import Path

import numpy as np
from PIL import Image

MODEL_URL = "https://github.com/ultralytics/assets/releases/download/v8.3.0/yolo11n.onnx"
MODEL_SHA256 = "634279b40c07c6391472c51ad45b81ebc48706a9a1fe72dd3396322acd0c053b"
INPUT_SIZE = 640
MIN_CONFIDENCE_MILLI = 600
MIN_AREA_MILLI = 10
MAX_TARGET_STEP = 100


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def download_model(path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    request = urllib.request.Request(
        MODEL_URL,
        headers={"User-Agent": "OpenRace-task3-yolo-fixture/1"},
    )
    temporary = path.with_suffix(path.suffix + ".tmp")
    with urllib.request.urlopen(request, timeout=120) as source, temporary.open("wb") as target:
        while chunk := source.read(1024 * 1024):
            target.write(chunk)
    actual = sha256(temporary)
    if actual != MODEL_SHA256:
        temporary.unlink(missing_ok=True)
        raise RuntimeError(f"downloaded model hash mismatch: {actual}")
    temporary.replace(path)


def letterbox(image: Image.Image) -> tuple[np.ndarray, float, int, int]:
    width, height = image.size
    scale = min(INPUT_SIZE / width, INPUT_SIZE / height)
    resized_width = max(1, round(width * scale))
    resized_height = max(1, round(height * scale))
    resized = image.resize((resized_width, resized_height), Image.Resampling.BILINEAR)
    canvas = Image.new("RGB", (INPUT_SIZE, INPUT_SIZE), (114, 114, 114))
    pad_left = (INPUT_SIZE - resized_width) // 2
    pad_top = (INPUT_SIZE - resized_height) // 2
    canvas.paste(resized, (pad_left, pad_top))
    array = np.asarray(canvas, dtype=np.float32) / 255.0
    return np.transpose(array, (2, 0, 1))[None, ...], scale, pad_left, pad_top


def iou(left: np.ndarray, right: np.ndarray) -> float:
    lx1, ly1 = left[0] - left[2] / 2, left[1] - left[3] / 2
    lx2, ly2 = left[0] + left[2] / 2, left[1] + left[3] / 2
    rx1, ry1 = right[0] - right[2] / 2, right[1] - right[3] / 2
    rx2, ry2 = right[0] + right[2] / 2, right[1] + right[3] / 2
    intersection = max(0.0, min(lx2, rx2) - max(lx1, rx1)) * max(
        0.0, min(ly2, ry2) - max(ly1, ry1)
    )
    union = left[2] * left[3] + right[2] * right[3] - intersection
    return intersection / union if union > 0 else 0.0


def decode_top(output: np.ndarray, original_size: tuple[int, int]) -> dict | None:
    """Decode one YOLO11 output and return the best post-NMS detection."""
    if output.ndim != 2 or output.shape[0] < 5:
        raise ValueError(f"unexpected YOLO output shape: {output.shape}")
    channels, rows = output.shape
    class_scores = output[4:, :]
    classes = class_scores.argmax(axis=0)
    scores = class_scores.max(axis=0)
    candidates = np.flatnonzero(scores >= MIN_CONFIDENCE_MILLI / 1000.0)
    if candidates.size == 0:
        return None

    order = candidates[np.argsort(scores[candidates])[::-1]]
    selected: list[int] = []
    for row in order:
        if all(classes[row] != classes[other] or iou(output[:4, row], output[:4, other]) <= 0.5 for other in selected):
            selected.append(int(row))
    row = selected[0]
    cx, cy, width, height = output[:4, row].astype(float)
    if not np.isfinite([cx, cy, width, height, scores[row]]).all() or width < 0 or height < 0:
        raise ValueError("YOLO output contains a non-finite or negative box")

    image_width, image_height = original_size
    scale = min(INPUT_SIZE / image_width, INPUT_SIZE / image_height)
    pad_left = (INPUT_SIZE - round(image_width * scale)) // 2
    pad_top = (INPUT_SIZE - round(image_height * scale)) // 2
    center_x = np.clip((cx - pad_left) / scale / image_width, 0.0, 1.0)
    center_y = np.clip((cy - pad_top) / scale / image_height, 0.0, 1.0)
    area = np.clip(width * height / (scale * scale * image_width * image_height), 0.0, 1.0)
    return {
        "class_id": int(classes[row]),
        "confidence_milli": int(np.clip(scores[row] * 1000 + 0.5, 0, 1000)),
        "center_x_milli": int(np.clip(center_x * 1000 + 0.5, 0, 1000)),
        "center_y_milli": int(np.clip(center_y * 1000 + 0.5, 0, 1000)),
        "area_milli": int(np.clip(area * 1000 + 0.5, 0, 1000)),
        "box_input_xywh": [float(value) for value in output[:4, row]],
        "source_row": row,
    }


def bounded_target(detection: dict | None, current_target: int) -> dict:
    if detection is None:
        return {"decision": "reject", "reason": "no_detection"}
    if detection["confidence_milli"] < MIN_CONFIDENCE_MILLI:
        return {"decision": "reject", "reason": "low_confidence"}
    if detection["area_milli"] < MIN_AREA_MILLI:
        return {"decision": "reject", "reason": "small_area"}
    mapped = detection["center_x_milli"]
    target = max(current_target - MAX_TARGET_STEP, min(current_target + MAX_TARGET_STEP, mapped))
    return {
        "decision": "target",
        "target": target,
        "class_id": detection["class_id"],
        "confidence_milli": detection["confidence_milli"],
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", type=Path, default=Path("tmp/task3-yolo/yolo11n.onnx"))
    parser.add_argument(
        "--image",
        type=Path,
        action="append",
        dest="images",
        help="fixture image; may be repeated",
    )
    parser.add_argument("--out-dir", type=Path, default=Path("results/task3/yolo"))
    parser.add_argument("--no-download", action="store_true")
    args = parser.parse_args()

    repo_root = Path(__file__).resolve().parents[2]
    images = args.images or [
        repo_root / "apps/starry/aka00-tennis-yolo/validation/tennis-ball-close.jpg",
        repo_root / "apps/starry/aka00-tennis-yolo/validation/tennis-ball-plant.jpg",
        repo_root / "apps/starry/aka00-tennis-yolo/validation/tennis-ball-black-box.jpg",
    ]
    if not args.model.exists():
        if args.no_download:
            raise SystemExit(f"model is missing and --no-download was set: {args.model}")
        download_model(args.model)
    actual_model_sha = sha256(args.model)
    if actual_model_sha != MODEL_SHA256:
        raise SystemExit(f"model hash mismatch: expected {MODEL_SHA256}, got {actual_model_sha}")

    try:
        import onnxruntime as ort
    except ImportError as error:
        raise SystemExit("onnxruntime is required; install it outside the repository") from error

    session = ort.InferenceSession(str(args.model), providers=["CPUExecutionProvider"])
    input_name = session.get_inputs()[0].name
    print(f"TASK3_MODEL_READY model=yolo11n.onnx sha256={actual_model_sha}")
    records = []
    for image_path in images:
        image = Image.open(image_path).convert("RGB")
        tensor, _, _, _ = letterbox(image)
        output = session.run(None, {input_name: tensor})[0][0]
        detection = decode_top(output, image.size)
        decision = bounded_target(detection, current_target=500)
        image_sha = sha256(image_path)
        try:
            display_path = str(image_path.resolve().relative_to(repo_root))
        except ValueError:
            display_path = str(image_path)
        record = {
            "image": display_path,
            "image_sha256": image_sha,
            "size": list(image.size),
            "output_shape": list(output.shape),
            "detection": detection,
            "decision": decision,
        }
        records.append(record)
        if detection is None:
            print(f"TASK3_MODEL_REJECTED image={image_path.name} reason=no_detection")
        else:
            print(
                "TASK3_DETECTION "
                f"image={image_path.name} class={detection['class_id']} "
                f"confidence_milli={detection['confidence_milli']} "
                f"center_x_milli={detection['center_x_milli']} "
                f"area_milli={detection['area_milli']} decision={decision['decision']}"
            )

    args.out_dir.mkdir(parents=True, exist_ok=True)
    manifest = {
        "model": {
            "name": "yolo11n.onnx",
            "url": MODEL_URL,
            "sha256": actual_model_sha,
            "input_size": [INPUT_SIZE, INPUT_SIZE],
        },
        "contract": {
            "min_confidence_milli": MIN_CONFIDENCE_MILLI,
            "min_area_milli": MIN_AREA_MILLI,
            "max_target_step": MAX_TARGET_STEP,
            "current_target": 500,
        },
        "records": records,
    }
    output_path = args.out_dir / "yolo-fixture-manifest.json"
    output_path.write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"TASK3_MODEL_MANIFEST path={output_path}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, ValueError) as error:
        print(f"TASK3_MODEL_ERROR {error}", file=sys.stderr)
        raise SystemExit(1) from error
