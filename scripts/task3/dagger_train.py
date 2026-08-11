#!/usr/bin/env python3
"""Closed-loop (DAgger-style) training for the Task-3 plant controller.

The teacher policy (perfect one-step inverse) drives the offline dataset, but
a model trained only on teacher trajectories can be unstable in closed loop
because its own outputs move the plant into regions the teacher never visited.

This script alternates between:
  1. training on the aggregated (teacher + previous closed-loop) dataset;
  2. simulating the fixed scenario *plus fresh random episodes* with the
     current model, asking the teacher for the correct output at each visited
     state, and appending those (state, teacher label) samples.

Only random episodes feed the training set; the fixed scenario is used once at
the end for the final evaluation so the reported number is not tuned against.
"""

import argparse
import hashlib
import json
import random
import struct
from pathlib import Path

import numpy as np
import torch
import torch.nn as nn

import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from generate_dataset import (  # noqa: E402
    EPISODE_MS,
    PERIOD_MS,
    build_arrays,
    generate_episode,
)
from features import FEATURES, WINDOW, build_window  # noqa: E402
from plant import (  # noqa: E402
    OUTPUT_MAX,
    OUTPUT_MIN,
    p_output,
    simulate,
    step,
    teacher_correction,
)

FEATURES = 4


class PlantCnn(nn.Module):
    def __init__(self, dtype=torch.float32):
        super().__init__()
        self.conv1 = nn.Conv1d(FEATURES, 32, kernel_size=5, padding=2, dtype=dtype)
        self.conv2 = nn.Conv1d(32, 64, kernel_size=5, padding=2, dtype=dtype)
        self.fc1 = nn.Linear(64, 32, dtype=dtype)
        self.fc2 = nn.Linear(32, 1, dtype=dtype)
        self.activation = nn.ReLU()

    def forward(self, x):
        x = self.activation(self.conv1(x))
        x = self.activation(self.conv2(x))
        x = x.mean(dim=2)
        x = self.activation(self.fc1(x))
        return self.fc2(x).squeeze(-1)


def make_model(weights_blob: bytes) -> PlantCnn:
    counts = {
        "conv1.weight": 640,
        "conv1.bias": 32,
        "conv2.weight": 10240,
        "conv2.bias": 64,
        "fc1.weight": 2048,
        "fc1.bias": 32,
        "fc2.weight": 32,
        "fc2.bias": 1,
    }
    shapes = {
        "conv1.weight": (32, 4, 5),
        "conv1.bias": (32,),
        "conv2.weight": (64, 32, 5),
        "conv2.bias": (64,),
        "fc1.weight": (32, 64),
        "fc1.bias": (32,),
        "fc2.weight": (1, 32),
        "fc2.bias": (1,),
    }
    model = PlantCnn()
    state = {}
    offset = 0
    for name in sorted(counts):
        n = counts[name]
        values = np.frombuffer(weights_blob[offset : offset + n * 8], dtype=np.float64)
        offset += n * 8
        state[name] = torch.from_numpy(
            np.array(values.reshape(shapes[name]), dtype=np.float32, copy=True)
        )
    model.load_state_dict(state)
    model.eval()
    return model


def freeze_blob(model) -> bytes:
    """Export the trained weights as a little-endian f64 blob; the Rust guest
    and the golden vectors both consume this exact precision."""
    f64_model = PlantCnn(dtype=torch.float64)
    f64_state = {
        name: tensor.detach().cpu().double() for name, tensor in model.state_dict().items()
    }
    f64_model.load_state_dict(f64_state)
    blob = b""
    for name in sorted(f64_model.state_dict()):
        values = np.ascontiguousarray(
            f64_model.state_dict()[name].detach().cpu().numpy(), dtype=np.float64
        ).reshape(-1)
        blob += struct.pack(f"<{len(values)}d", *values.tolist())
    return blob


def closed_loop_samples(model, rng, episodes):
    """Roll the current model out against the real plant simulator and return
    every visited (state window, teacher correction) pair.

    This is the actual closed loop: the model output feeds `plant.step` and
    the resulting state feeds the next window, so the aggregated data covers
    the states the model itself visits (the DAgger distribution)."""
    features, labels = [], []
    for samples, _target_steps, _disturbance_steps in episodes:
        state_history = []
        output_history = []
        prev_output = 0
        state = samples[0]["state_before"]
        targets = [s["target"] for s in samples]
        disturbances = [s["disturbance"] for s in samples]
        for index in range(len(samples)):
            target = targets[index]
            disturbance = disturbances[index]
            # Single shared feature builder (see features.py) so the DAgger
            # distribution matches the base dataset and the Rust guest.
            window = build_window(
                state_history,
                output_history,
                target,
                state,
                prev_output,
            )
            feat = window.T[np.newaxis, ...]
            if index >= WINDOW:
                label = teacher_correction(state, target, disturbance)
                features.append(window.copy())
                labels.append(label)
            with torch.no_grad():
                correction = float(model(torch.from_numpy(feat).float()).numpy()[0]) * 1000.0
            # Residual policy: the frozen P term keeps the loop stable, the
            # model adds the learned loss/disturbance compensation.
            output = int(
                min(
                    OUTPUT_MAX,
                    max(
                        OUTPUT_MIN,
                        round(p_output(state, target) + correction),
                    ),
                )
            )
            prev_output = output
            state_history.append(state)
            state_history = state_history[-WINDOW:]
            output_history.append(output)
            output_history = output_history[-WINDOW:]
            state = step(state, output, disturbance)
    return np.stack(features), np.asarray(labels, dtype=np.float64)


def fixed_scenario_rmse(model) -> float:
    """Closed-loop RMSE on the frozen fixed scenario (evaluation only, never
    added to the training set)."""

    def controller(context):
        state = context["state"]
        target = context["target"]
        window = build_window(
            context["state_history"],
            context["output_history"],
            target,
            state,
            context["prev_output"],
        )
        feat = window.T[np.newaxis, ...]
        with torch.no_grad():
            correction = float(model(torch.from_numpy(feat).float()).numpy()[0]) * 1000.0
        return int(
            min(
                OUTPUT_MAX,
                max(OUTPUT_MIN, round(p_output(state, target) + correction)),
            )
        )

    samples = simulate(controller=controller)
    return float(
        (np.mean([(s["target"] - s["state_after"]) ** 2 for s in samples])) ** 0.5
    )


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--data-dir", type=Path, default=Path("tmp/task3-data"))
    parser.add_argument("--repo-root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--iterations", type=int, default=8)
    parser.add_argument("--epochs", type=int, default=40)
    parser.add_argument("--batch-size", type=int, default=256)
    parser.add_argument("--lr", type=float, default=1e-3)
    parser.add_argument("--seed", type=int, default=20260811)
    parser.add_argument("--extra-episodes", type=int, default=120)
    parser.add_argument("--closed-loop-weight", type=int, default=4)
    args = parser.parse_args()

    rng = random.Random(args.seed + 2)
    torch.manual_seed(args.seed)
    np.random.seed(args.seed)

    base_train = np.load(args.data_dir / "train.npz")
    val = np.load(args.data_dir / "val.npz")
    val_x = torch.from_numpy(val["features"]).float().transpose(1, 2)
    val_y = torch.from_numpy(val["labels"]).float() / 1000.0

    model_dir = args.repo_root / "components" / "task3-model" / "model"
    weights_path = model_dir / "weights.bin"
    model = make_model(weights_path.read_bytes())
    criterion = nn.MSELoss()

    agg_features = [base_train["features"]]
    agg_labels = [base_train["labels"]]

    for iteration in range(args.iterations):
        episodes = [generate_episode(rng) for _ in range(args.extra_episodes)]
        closed_features, closed_labels = closed_loop_samples(model, rng, episodes)
        # The newest closed-loop batch is the most informative for the current
        # failure regions; oversample it so the update step cannot drown it in
        # the accumulated teacher data.
        for _ in range(args.closed_loop_weight):
            agg_features.append(closed_features)
            agg_labels.append(closed_labels)
        features = np.concatenate(agg_features)
        labels = np.concatenate(agg_labels)
        train_x = torch.from_numpy(features).float().transpose(1, 2)
        train_y = torch.from_numpy(labels).float() / 1000.0

        optimizer = torch.optim.AdamW(model.parameters(), lr=args.lr)
        model.train()
        for epoch in range(args.epochs):
            permutation = torch.randperm(train_x.shape[0])
            for start in range(0, train_x.shape[0], args.batch_size):
                indices = permutation[start : start + args.batch_size]
                optimizer.zero_grad()
                loss = criterion(model(train_x[indices]), train_y[indices])
                loss.backward()
                optimizer.step()
        model.eval()
        with torch.no_grad():
            val_loss = criterion(model(val_x), val_y).item()
        scenario_rmse = fixed_scenario_rmse(model)
        print(
            f"iteration {iteration + 1}/{args.iterations} closed_samples={len(closed_labels)} "
            f"val_mse={val_loss:.6f} fixed_scenario_rmse={scenario_rmse:.1f}"
        )

    blob = freeze_blob(model)
    weights_path.write_bytes(blob)
    _update_model_manifest(model_dir, weights_path, blob, args)
    print(f"wrote {weights_path} ({weights_path.stat().st_size} bytes)")


def _update_model_manifest(
    model_dir: Path, weights_path: Path, blob: bytes, args: argparse.Namespace
) -> None:
    """Refresh model.json so its hashes describe the DAgger-final artifact.

    train_model.py writes model.json for its intermediate weights; DAgger then
    overwrites weights.bin, so this function updates the manifest in place
    (structure/normalization are unchanged) and drops the pre-DAgger metric
    fields that no longer describe the committed blob.
    """
    manifest = json.loads((model_dir / "model.json").read_text())
    manifest.update(
        {
            "weights_sha256": hashlib.sha256(blob).hexdigest(),
            "weights_bytes": len(blob),
            "trainer": "dagger_train.py",
            "trainer_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
            "seed": args.seed,
            "dagger_iterations": args.iterations,
            "dagger_epochs": args.epochs,
            "dagger_closed_loop_weight": args.closed_loop_weight,
        }
    )
    for key in ("best_val_mse", "val_label_mae", "epochs"):
        manifest.pop(key, None)
    (model_dir / "model.json").write_text(json.dumps(manifest, indent=2) + "\n")


if __name__ == "__main__":
    main()
