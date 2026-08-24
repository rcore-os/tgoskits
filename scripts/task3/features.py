"""Single source of truth for the Task-3 model input window.

The frozen feature contract (book/design/task3-ai-control-todo.md M0):

  window: 64 samples x 4 channels [state, target, error, prev_output]
  - state        history of plant states, chronological, oldest first;
                 before 64 samples exist the window is padded at the front
                 with the first available state
  - target       the *current* target, constant across the window (the
                 controller always knows it)
  - error        current_target - state, per sample point
  - prev_output  the applied control output per sample point, chronological,
                 padded at the front with the first available output

Every consumer must use build_window(): dataset generation, DAgger rollouts,
offline evaluation, and the Rust guest (which mirrors it with golden tests in
components/task3-model/model/golden_vectors.rs).
"""

from __future__ import annotations

import numpy as np

WINDOW = 64
FEATURES = 4
SCALE = 1000.0


def build_window(
    state_history: list[int],
    output_history: list[int],
    target: int,
    fallback_state: int,
    fallback_output: int,
) -> np.ndarray:
    """Build the (WINDOW, FEATURES) normalized window.

    `state_history` / `output_history` are chronological with the most recent
    sample last.  Windows shorter than WINDOW are padded at the front with the
    first available value (fallbacks when the history is empty), matching the
    deployment behaviour of the Linux guest controller.
    """
    states = padded_window(state_history, fallback_state)
    outputs = padded_window(output_history, fallback_output)
    window = np.zeros((WINDOW, FEATURES), dtype=np.float64)
    for index in range(WINDOW):
        state = states[index]
        window[index, 0] = state / SCALE
        window[index, 1] = target / SCALE
        window[index, 2] = (target - state) / SCALE
        window[index, 3] = outputs[index] / SCALE
    return window


def padded_window(history: list[int], fallback: int) -> list[int]:
    values = history[-WINDOW:] if history else [fallback]
    if len(values) < WINDOW:
        values = [values[0]] * (WINDOW - len(values)) + values
    return values
