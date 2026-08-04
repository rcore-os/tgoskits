//! Deterministic thermal controller and comparison scenario.
//!
//! The small dense network is deliberately dependency-free so the exact model
//! runs identically in a Linux guest and in host-side acceptance tests.

use thiserror::Error;

#[cfg(test)]
use crate::neural_model_generated::GOLDEN_CASES;
pub use crate::neural_model_generated::{MODEL_ID, MODEL_SOURCE_SHA256};
use crate::{
    control::{
        ControlCommand, ControlMode, ControlOperation, MAX_ACTUATOR_PERMILLE,
        MAX_TEMPERATURE_MILLI_C, MIN_TEMPERATURE_MILLI_C,
    },
    neural_model_generated::{
        ACTUATOR_INPUT_SCALE, ACTUATOR_OUTPUT_SCALE, ACTUATOR_ROUNDING_HALF, ERROR_SCALE, HIDDEN,
        HIDDEN_BIASES, HIDDEN_WEIGHTS, INPUTS, OUTPUT_BIAS, OUTPUT_MAX, OUTPUT_MIN, OUTPUT_WEIGHTS,
        RATE_SCALE, SETPOINT_OFFSET_MILLI_C, SETPOINT_SCALE,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ThermalObservation {
    pub temperature_milli_c: i32,
    pub setpoint_milli_c: i32,
    pub previous_actuator_permille: u16,
    pub temperature_rate_milli_c_per_s: i32,
}

impl ThermalObservation {
    pub fn normalized(self) -> Result<[f32; INPUTS], NeuralError> {
        if !(MIN_TEMPERATURE_MILLI_C..=MAX_TEMPERATURE_MILLI_C).contains(&self.temperature_milli_c)
        {
            return Err(NeuralError::TemperatureOutOfRange(self.temperature_milli_c));
        }
        if !(MIN_TEMPERATURE_MILLI_C..=MAX_TEMPERATURE_MILLI_C).contains(&self.setpoint_milli_c) {
            return Err(NeuralError::SetpointOutOfRange(self.setpoint_milli_c));
        }
        if self.previous_actuator_permille > MAX_ACTUATOR_PERMILLE {
            return Err(NeuralError::ActuatorOutOfRange(
                self.previous_actuator_permille,
            ));
        }
        if !(-100_000..=100_000).contains(&self.temperature_rate_milli_c_per_s) {
            return Err(NeuralError::RateOutOfRange(
                self.temperature_rate_milli_c_per_s,
            ));
        }
        Ok([
            (self.setpoint_milli_c - self.temperature_milli_c) as f32 / ERROR_SCALE,
            (self.setpoint_milli_c - SETPOINT_OFFSET_MILLI_C) as f32 / SETPOINT_SCALE,
            self.temperature_rate_milli_c_per_s as f32 / RATE_SCALE,
            f32::from(self.previous_actuator_permille) / ACTUATOR_INPUT_SCALE,
        ])
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NeuralController;

impl NeuralController {
    pub fn command(
        self,
        observation: ThermalObservation,
        sample_id: u32,
    ) -> Result<ControlCommand, NeuralError> {
        let output = self.infer_normalized(observation.normalized()?)?;
        // The clamped output is nonnegative, so adding one half implements
        // round-to-nearest without requiring a platform math library.
        let actuator_permille = (output * ACTUATOR_OUTPUT_SCALE + ACTUATOR_ROUNDING_HALF) as u16;
        Ok(ControlCommand {
            operation: ControlOperation::SetActuator,
            mode: ControlMode::Neural,
            actuator_permille,
            setpoint_milli_c: observation.setpoint_milli_c,
            sample_id,
        })
    }

    pub fn infer_normalized(self, inputs: [f32; INPUTS]) -> Result<f32, NeuralError> {
        if inputs.iter().any(|input| !input.is_finite()) {
            return Err(NeuralError::NonFiniteInput);
        }
        let mut hidden = [0.0f32; HIDDEN];
        for (neuron, output) in hidden.iter_mut().enumerate() {
            let weighted_sum = HIDDEN_WEIGHTS[neuron]
                .iter()
                .zip(inputs)
                .fold(HIDDEN_BIASES[neuron], |sum, (weight, input)| {
                    sum + weight * input
                });
            *output = weighted_sum.max(0.0);
        }
        let output = OUTPUT_WEIGHTS
            .iter()
            .zip(hidden)
            .fold(OUTPUT_BIAS, |sum, (weight, activation)| {
                sum + weight * activation
            });
        if !output.is_finite() {
            return Err(NeuralError::NonFiniteOutput);
        }
        Ok(output.clamp(OUTPUT_MIN, OUTPUT_MAX))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManualFixedController {
    actuator_permille: u16,
}

impl ManualFixedController {
    pub fn new(actuator_permille: u16) -> Result<Self, NeuralError> {
        if actuator_permille > MAX_ACTUATOR_PERMILLE {
            return Err(NeuralError::ActuatorOutOfRange(actuator_permille));
        }
        Ok(Self { actuator_permille })
    }

    pub fn command(self, observation: ThermalObservation, sample_id: u32) -> ControlCommand {
        ControlCommand {
            operation: ControlOperation::SetActuator,
            mode: ControlMode::ManualFixed,
            actuator_permille: self.actuator_permille,
            setpoint_milli_c: observation.setpoint_milli_c,
            sample_id,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Policy {
    ManualFixed { actuator_permille: u16 },
    Neural,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScenarioMetrics {
    pub samples: u32,
    pub rmse_milli_c: f64,
    pub iae_milli_c_s: f64,
    pub maximum_overshoot_milli_c: i32,
    pub final_settling_time_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScenarioSample {
    pub step: u32,
    pub elapsed_ms: u64,
    pub setpoint_milli_c: i32,
    pub measured_milli_c: i32,
    pub actuator_permille: u16,
    pub error_milli_c: i32,
}

/// Runs the same deterministic plant and setpoint sequence for either policy.
pub fn evaluate_policy(policy: Policy) -> Result<ScenarioMetrics, NeuralError> {
    evaluate_policy_with_observer(policy, |_| {})
}

/// Runs the comparison scenario and emits every raw sample to `observer`.
pub fn evaluate_policy_with_observer(
    policy: Policy,
    mut observer: impl FnMut(ScenarioSample),
) -> Result<ScenarioMetrics, NeuralError> {
    const STEP_MS: u64 = 100;
    const STEPS: u32 = 1_800;
    const FINAL_SEGMENT_START: u32 = 1_200;
    const SETTLING_WINDOW_STEPS: u32 = 50;
    const SETTLING_BAND_MILLI_C: i32 = 1_000;

    let mut plant = ThermalPlant::new(20_000);
    let mut previous_temperature = plant.temperature_milli_c();
    let mut actuator = 0u16;
    let mut sum_squared_error = 0f64;
    let mut integrated_absolute_error = 0f64;
    let mut maximum_overshoot = 0i32;
    let mut consecutive_inside = 0u32;
    let mut settling_time_ms = None;

    for step in 0..STEPS {
        let setpoint = scenario_setpoint(step);
        let temperature = plant.temperature_milli_c();
        let rate = (temperature - previous_temperature) * 10;
        let observation = ThermalObservation {
            temperature_milli_c: temperature,
            setpoint_milli_c: setpoint,
            previous_actuator_permille: actuator,
            temperature_rate_milli_c_per_s: rate,
        };
        let command = match policy {
            Policy::ManualFixed { actuator_permille } => {
                ManualFixedController::new(actuator_permille)?.command(observation, step + 1)
            }
            Policy::Neural => NeuralController.command(observation, step + 1)?,
        };
        actuator = command.actuator_permille;
        previous_temperature = temperature;
        plant.step(actuator, step);

        let measured = plant.temperature_milli_c();
        let error = i64::from(setpoint) - i64::from(measured);
        observer(ScenarioSample {
            step,
            elapsed_ms: u64::from(step + 1) * STEP_MS,
            setpoint_milli_c: setpoint,
            measured_milli_c: measured,
            actuator_permille: actuator,
            error_milli_c: error as i32,
        });
        sum_squared_error += (error * error) as f64;
        integrated_absolute_error += error.unsigned_abs() as f64 * (STEP_MS as f64 / 1_000.0);
        maximum_overshoot = maximum_overshoot.max(measured - setpoint);

        if step >= FINAL_SEGMENT_START {
            if error.unsigned_abs() <= SETTLING_BAND_MILLI_C as u64 {
                consecutive_inside += 1;
                if consecutive_inside == SETTLING_WINDOW_STEPS {
                    let first_inside_step = step + 1 - SETTLING_WINDOW_STEPS;
                    settling_time_ms =
                        Some(u64::from(first_inside_step - FINAL_SEGMENT_START) * STEP_MS);
                }
            } else {
                consecutive_inside = 0;
                settling_time_ms = None;
            }
        }
    }

    Ok(ScenarioMetrics {
        samples: STEPS,
        rmse_milli_c: square_root(sum_squared_error / f64::from(STEPS)),
        iae_milli_c_s: integrated_absolute_error,
        maximum_overshoot_milli_c: maximum_overshoot,
        final_settling_time_ms: settling_time_ms,
    })
}

fn scenario_setpoint(step: u32) -> i32 {
    match step {
        0..=599 => 45_000,
        600..=1_199 => 65_000,
        _ => 50_000,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ThermalPlant {
    temperature_c: f32,
}

impl ThermalPlant {
    pub fn new(temperature_milli_c: i32) -> Self {
        Self {
            temperature_c: temperature_milli_c as f32 / 1_000.0,
        }
    }

    pub fn temperature_milli_c(self) -> i32 {
        let scaled = self.temperature_c * 1_000.0;
        if scaled >= 0.0 {
            (scaled + 0.5) as i32
        } else {
            (scaled - 0.5) as i32
        }
    }

    pub fn step(&mut self, actuator_permille: u16, step: u32) {
        const AMBIENT_C: f32 = 20.0;
        const HEATER_C_PER_S: f32 = 2.8;
        const COOLING_PER_S: f32 = 0.04;
        const DT_S: f32 = 0.1;

        let actuator = f32::from(actuator_permille) / 1_000.0;
        let disturbance = if (850..950).contains(&step) {
            -0.35
        } else {
            0.0
        };
        let derivative = HEATER_C_PER_S * actuator
            - COOLING_PER_S * (self.temperature_c - AMBIENT_C)
            + disturbance;
        self.temperature_c += derivative * DT_S;
    }
}

fn square_root(value: f64) -> f64 {
    if value <= 0.0 {
        return 0.0;
    }
    let mut estimate = if value >= 1.0 { value } else { 1.0 };
    for _ in 0..32 {
        estimate = (estimate + value / estimate) * 0.5;
    }
    estimate
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum NeuralError {
    #[error("temperature {0} mC is outside model range")]
    TemperatureOutOfRange(i32),
    #[error("setpoint {0} mC is outside model range")]
    SetpointOutOfRange(i32),
    #[error("actuator {0} is outside 0..=1000 permille")]
    ActuatorOutOfRange(u16),
    #[error("temperature rate {0} mC/s is outside model range")]
    RateOutOfRange(i32),
    #[error("model input contains NaN or infinity")]
    NonFiniteInput,
    #[error("model output is NaN or infinity")]
    NonFiniteOutput,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inference_is_deterministic() {
        let observation = ThermalObservation {
            temperature_milli_c: 40_000,
            setpoint_milli_c: 55_000,
            previous_actuator_permille: 400,
            temperature_rate_milli_c_per_s: 1_000,
        };
        assert_eq!(
            NeuralController.command(observation, 7).unwrap(),
            ControlCommand {
                operation: ControlOperation::SetActuator,
                mode: ControlMode::Neural,
                actuator_permille: 826,
                setpoint_milli_c: 55_000,
                sample_id: 7,
            }
        );
    }

    #[test]
    fn nonfinite_model_input_is_rejected() {
        assert_eq!(
            NeuralController.infer_normalized([f32::NAN, 0.0, 0.0, 0.0]),
            Err(NeuralError::NonFiniteInput)
        );
    }

    #[test]
    fn generated_native_constants_match_frozen_golden_prefix() {
        for (input_bits, expected_output_bits, expected_actuator) in GOLDEN_CASES {
            let output = NeuralController
                .infer_normalized(input_bits.map(f32::from_bits))
                .unwrap();
            assert_eq!(output.to_bits(), expected_output_bits);
            assert_eq!(
                (output * ACTUATOR_OUTPUT_SCALE + ACTUATOR_ROUNDING_HALF) as u16,
                expected_actuator
            );
        }
    }

    #[test]
    fn neural_policy_improves_both_primary_metrics() {
        let manual = evaluate_policy(Policy::ManualFixed {
            actuator_permille: 500,
        })
        .unwrap();
        let neural = evaluate_policy(Policy::Neural).unwrap();
        assert!(neural.rmse_milli_c < manual.rmse_milli_c * 0.8);
        assert!(neural.iae_milli_c_s < manual.iae_milli_c_s * 0.8);
        assert!(neural.final_settling_time_ms.is_some());
    }
}
