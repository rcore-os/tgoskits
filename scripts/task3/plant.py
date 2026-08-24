"""Task-3 virtual plant simulator with Zephyr-identical integer semantics.

The RTOS Guest implements the plant with C integer arithmetic:

    loss(state)    = PLANT_BASE_LOSS
                     + PLANT_NONLINEAR_LOSS * state * state / 1_000_000
    response       = PLANT_RESPONSE * (output - state) / PLANT_RESPONSE_DEN
    state_next     = clamp(state + response - loss(state) + disturbance, 0, 1000)

C division truncates toward zero, which differs from Python floor division,
so every division here mirrors the C behaviour via math.trunc.  This module is
used by both dataset generation and metric verification so the host simulation
and the on-device plant agree sample by sample.
"""

from __future__ import annotations

import math

STATE_MIN = 0
STATE_MAX = 1000
OUTPUT_MIN = 0
OUTPUT_MAX = 1000
BASE_LOSS = 15
NONLINEAR_LOSS = 120
RESPONSE = 350
RESPONSE_DEN = 1000

# Frozen fixed scenario (M0): target steps and disturbance schedule.
TARGET_STEPS = [(0, 300), (5_000, 800), (15_000, 500)]
DISTURBANCE_STEPS = [(8_000, 150), (17_000, 0)]


def trunc_div(a: int, b: int) -> int:
    """C-style integer division truncating toward zero."""
    return math.trunc(a / b)


def loss(state: int) -> int:
    return BASE_LOSS + trunc_div(NONLINEAR_LOSS * state * state, 1_000_000)


def response(output: int, state: int) -> int:
    return trunc_div(RESPONSE * (output - state), RESPONSE_DEN)


def step(state: int, output: int, disturbance: int) -> int:
    """One plant update; returns the next state (Zephyr semantics)."""
    return min(OUTPUT_MAX, max(STATE_MIN, state + response(output, state) - loss(state) + disturbance))


def teacher_output(state: int, target: int, disturbance: int, gain: float = 0.5) -> int:
    """Model-based tracking policy: a one-step inverse controller aimed a
    fraction `gain` of the way to the target.

        target_frac = state + gain * (target - state)
        u           = state + (loss(state) - disturbance + target_frac - state)
                       * DEN / RESPONSE

    With the full gain (1.0) this is bang-bang style perfect one-step tracking,
    which is hard for a learned policy to imitate stably in closed loop.  The
    frozen gain 0.5 produces exponential, non-overshooting tracking while still
    requiring the model to recover the unobservable disturbance from history.
    Used only as the supervised label during offline training.
    """
    target_frac = state + gain * (target - state)
    numerator = loss(state) - disturbance + (target_frac - state)
    output = state + trunc_div(round(numerator * RESPONSE_DEN), RESPONSE)
    return min(OUTPUT_MAX, max(OUTPUT_MIN, output))


def p_output(state: int, target: int) -> int:
    """Frozen Task-3 baseline P controller (Kp=2, bias=0)."""
    return min(OUTPUT_MAX, max(OUTPUT_MIN, 2 * (target - state)))


def teacher_correction(state: int, target: int, disturbance: int, gain: float = 0.5) -> int:
    """Residual label: how much the teacher output exceeds the baseline P
    controller at the same state.  The model learns this bounded correction
    (loss/disturbance feedforward) instead of the full control law, so the P
    term keeps the closed loop stable even where the model is inaccurate."""
    return teacher_output(state, target, disturbance, gain) - p_output(state, target)


def target_at(elapsed_ms: int, steps: list[tuple[int, int]] | None = None) -> int:
    steps = steps or TARGET_STEPS
    value = steps[0][1]
    for start_ms, step_value in steps:
        if elapsed_ms >= start_ms:
            value = step_value
    return value


def disturbance_at(elapsed_ms: int, steps: list[tuple[int, int]] | None = None) -> int:
    steps = steps or DISTURBANCE_STEPS
    value = 0
    for start_ms, step_value in steps:
        if elapsed_ms >= start_ms:
            value = step_value
    return value


def simulate(
    initial_state: int = 300,
    duration_ms: int = 30_000,
    period_ms: int = 100,
    target_steps: list[tuple[int, int]] | None = None,
    disturbance_steps: list[tuple[int, int]] | None = None,
    controller=None,
):
    """Closed-loop simulation over the frozen scenario.

    `controller(context)` returns the output to apply, where `context` is a
    dict with `state`, `target`, `prev_output`, `state_history`,
    `output_history` (chronological, last = most recent) and `elapsed_ms`.
    If None, a fixed P controller with the frozen Kp=2 is used, matching the
    Task-3 baseline.
    """
    state = initial_state
    prev_output = 0
    state_history: list[int] = []
    output_history: list[int] = []
    samples = []
    for elapsed_ms in range(0, duration_ms + 1, period_ms):
        target = target_at(elapsed_ms, target_steps)
        disturbance = disturbance_at(elapsed_ms, disturbance_steps)
        if controller is None:
            output = min(1000, max(0, 2 * (target - state)))
        else:
            output = controller(
                {
                    "state": state,
                    "target": target,
                    "prev_output": prev_output,
                    "state_history": state_history,
                    "output_history": output_history,
                    "elapsed_ms": elapsed_ms,
                }
            )
        state_next = step(state, output, disturbance)
        samples.append(
            {
                "elapsed_ms": elapsed_ms,
                "state_before": state,
                "target": target,
                "output": output,
                "state_after": state_next,
                "disturbance": disturbance,
                "prev_output": prev_output,
            }
        )
        state_history.append(state)
        state_history = state_history[-64:]
        output_history.append(output)
        output_history = output_history[-64:]
        prev_output = output
        state = state_next
    return samples


if __name__ == "__main__":
    # Cross-check against the recorded M2 closed-loop log: CONTROL 0 from
    # state 300 must produce 170; CONTROL 260 from 170 must produce 183.
    assert step(300, 0, 0) == 170, step(300, 0, 0)
    assert step(170, 260, 0) == 183, step(170, 260, 0)
    assert step(181, 236, 0) == 182, step(181, 236, 0)
    print("plant simulator matches Zephyr integer semantics")
