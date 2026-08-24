#!/usr/bin/env python3
"""Generate the Task-3 offline training/validation dataset.

Episodes randomize the target schedule and the disturbance schedule so the
model learns to compensate the nonlinear loss and load steps in general; the
frozen fixed scenario (M0) is deliberately excluded from training and used
only for evaluation.

Each episode is simulated with the Zephyr-identical integer plant.  Every
timestep produces a window of 64 history samples with the four features
[state, target, error, prev_output] normalised to ~[-1, 1] and the teacher
label (perfect one-step inverse control output) for supervised training.
"""

import argparse
import json
import random
from pathlib import Path

import numpy as np

from plant import (
    OUTPUT_MIN,
    OUTPUT_MAX,
    p_output,
    simulate,
    step,
    teacher_correction,
)
from features import FEATURES, WINDOW, build_window

FEATURE_NAMES = ["state", "target", "error", "prev_output"]
EPISODE_MS = 30_000
PERIOD_MS = 100


def random_steps(rng: random.Random, count: int, start_ms: int, end_ms: int):
    """Random non-overlapping step schedule within [start, end)."""
    steps = []
    cursor = start_ms
    for index in range(count):
        duration = rng.randint(3_000, 8_000)
        if cursor + duration > end_ms:
            break
        steps.append((cursor, rng.randint(120, 950)))
        cursor += duration
    steps.append((cursor, rng.randint(120, 950)))
    return steps


def random_steps_hold(rng: random.Random, end_ms: int):
    """Constant-target episode so the model learns steady-state compensation
    (the dominant regime of the frozen scenario)."""
    return [(0, rng.randint(120, 950))]


def random_disturbance(rng: random.Random):
    steps = []
    if rng.random() < 0.85:
        on = rng.randint(2_000, 18_000)
        duration = rng.randint(3_000, 10_000)
        magnitude = rng.choice([-150, 150, -100, 100])
        steps = [(on, magnitude), (on + duration, 0)]
    return steps


def generate_episode(rng: random.Random):
    if rng.random() < 0.35:
        target_steps = random_steps_hold(rng, EPISODE_MS)
    else:
        target_steps = random_steps(rng, rng.randint(3, 5), 0, EPISODE_MS)
    disturbance_steps = random_disturbance(rng)
    state = rng.randint(0, 500)
    samples = simulate(
        initial_state=state,
        duration_ms=EPISODE_MS,
        period_ms=PERIOD_MS,
        target_steps=target_steps,
        disturbance_steps=disturbance_steps,
    )
    return samples, target_steps, disturbance_steps


def build_arrays(episodes, noise_rng: random.Random | None = None):
    features, labels = [], []
    for samples, _, _ in episodes:
        states = [s["state_after"] for s in samples]
        targets = [s["target"] for s in samples]
        outputs = [s["output"] for s in samples]
        for index in range(WINDOW, len(samples)):
            state_hist = states[index - WINDOW : index]
            output_hist = outputs[index - WINDOW : index]
            # Deployment feeds back the model's own outputs, while training
            # only sees teacher outputs; perturb the channel so the model does
            # not overfit to the exact teacher trajectory.
            if noise_rng is not None:
                output_hist = [
                    int(
                        min(
                            1000,
                            max(
                                0,
                                round(
                                    output
                                    * (1.0 + noise_rng.uniform(-0.06, 0.06))
                                ),
                            ),
                        )
                    )
                    for output in output_hist
                ]
            # Single shared feature builder (see features.py): constant target
            # channel, per-sample-point error and previous-output channels.
            row = build_window(
                state_hist,
                output_hist,
                targets[index],
                states[0],
                outputs[0],
            )
            label = teacher_correction(
                states[index - 1], targets[index], samples[index]["disturbance"]
            )
            features.append(row)
            labels.append(label)
    return np.stack(features), np.asarray(labels, dtype=np.float64)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--train-episodes", type=int, default=200)
    parser.add_argument("--val-episodes", type=int, default=30)
    parser.add_argument("--seed", type=int, default=20260811)
    parser.add_argument("--out-dir", type=Path, default=Path("tmp/task3-data"))
    args = parser.parse_args()

    rng = random.Random(args.seed)
    train_episodes = [generate_episode(rng) for _ in range(args.train_episodes)]
    val_episodes = [generate_episode(rng) for _ in range(args.val_episodes)]
    noise_rng = random.Random(args.seed + 1)

    train_features, train_labels = build_arrays(train_episodes, noise_rng)
    val_features, val_labels = build_arrays(val_episodes)

    args.out_dir.mkdir(parents=True, exist_ok=True)
    np.savez_compressed(
        args.out_dir / "train.npz",
        features=train_features,
        labels=train_labels,
    )
    np.savez_compressed(
        args.out_dir / "val.npz",
        features=val_features,
        labels=val_labels,
    )
    manifest = {
        "seed": args.seed,
        "window": WINDOW,
        "features": FEATURE_NAMES,
        "period_ms": PERIOD_MS,
        "episode_ms": EPISODE_MS,
        "train_episodes": args.train_episodes,
        "val_episodes": args.val_episodes,
        "train_samples": int(train_labels.shape[0]),
        "val_samples": int(val_labels.shape[0]),
        "label_range": [int(train_labels.min()), int(train_labels.max())],
        "label": "teacher_correction(state,target,disturbance) - p_output(state,target)",
        "fixed_scenario_in_training": False,
    }
    (args.out_dir / "dataset.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps(manifest, indent=2))


if __name__ == "__main__":
    main()
