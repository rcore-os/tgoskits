use std::{env, process, time::Instant};

const PREBUILD_MARKER: &str = include_str!(concat!(env!("OUT_DIR"), "/prebuild_marker.txt"));

#[derive(Copy, Clone)]
struct Sample {
    demand: i32,
    load: i32,
    vibration: i32,
}

const SAMPLES: [Sample; 8] = [
    Sample {
        demand: 930,
        load: 30,
        vibration: 5,
    },
    Sample {
        demand: 1000,
        load: 55,
        vibration: 12,
    },
    Sample {
        demand: 1080,
        load: 70,
        vibration: 9,
    },
    Sample {
        demand: 1150,
        load: 82,
        vibration: 18,
    },
    Sample {
        demand: 970,
        load: 40,
        vibration: 20,
    },
    Sample {
        demand: 1040,
        load: 63,
        vibration: 7,
    },
    Sample {
        demand: 1120,
        load: 75,
        vibration: 15,
    },
    Sample {
        demand: 990,
        load: 48,
        vibration: 11,
    },
];

const MANUAL_PWM: i32 = 650;
const INPUTS: usize = 4;
const HIDDEN: usize = 4;
const HIDDEN_WEIGHTS: [[i32; INPUTS]; HIDDEN] =
    [[1, 0, 0, 0], [0, 1, 0, 0], [0, 0, 1, 0], [0, 0, 0, 1]];
const HIDDEN_BIASES: [i32; HIDDEN] = [0, 0, 0, 0];
const OUTPUT_WEIGHTS: [i32; HIDDEN] = [1, 2, 1, -400];
const OUTPUT_BIAS: i32 = 0;

fn abs(v: i32) -> i32 {
    if v < 0 { -v } else { v }
}

fn plant_output(pwm: i32, s: Sample) -> i32 {
    400 + pwm - 2 * s.load - s.vibration
}

fn relu(v: i32) -> i32 {
    v.max(0)
}

fn dot<const N: usize>(weights: &[i32; N], inputs: &[i32; N]) -> i32 {
    weights
        .iter()
        .zip(inputs.iter())
        .map(|(weight, input)| weight * input)
        .sum()
}

fn infer_pwm(s: Sample) -> i32 {
    let inputs = [s.demand, s.load, s.vibration, 1];
    let mut hidden = [0; HIDDEN];
    for (idx, weights) in HIDDEN_WEIGHTS.iter().enumerate() {
        hidden[idx] = relu(dot(weights, &inputs) + HIDDEN_BIASES[idx]);
    }
    (dot(&OUTPUT_WEIGHTS, &hidden) + OUTPUT_BIAS).clamp(0, 2_000)
}

fn main() {
    let marker = PREBUILD_MARKER.trim();

    println!(
        "REDCOLA_STARRY_AI_BEGIN guest=StarryOS role=non_rt_guest model=fixed_point_mlp_policy \
         hidden={} samples={} pid={} prebuild_marker={}",
        HIDDEN,
        SAMPLES.len(),
        process::id(),
        marker
    );
    println!(
        "redcola-ai-control args={:?}",
        env::args().collect::<Vec<_>>()
    );

    let mut manual_abs_error = 0;
    let mut ai_abs_error = 0;
    let mut max_ai_error = 0;
    let mut infer_total_us: u128 = 0;

    for (idx, s) in SAMPLES.iter().copied().enumerate() {
        let start = Instant::now();
        let ai_pwm = infer_pwm(s);
        let infer_us = start.elapsed().as_micros();
        infer_total_us += infer_us;
        let manual_out = plant_output(MANUAL_PWM, s);
        let ai_out = plant_output(ai_pwm, s);
        let manual_error = abs(s.demand - manual_out);
        let ai_error = abs(s.demand - ai_out);
        manual_abs_error += manual_error;
        ai_abs_error += ai_error;
        max_ai_error = max_ai_error.max(ai_error);
        println!(
            "REDCOLA_STARRY_AI_SAMPLE seq={} demand={} load={} vibration={} manual_pwm={} \
             ai_pwm={} manual_error={} ai_error={} nn_infer_us={}",
            idx + 1,
            s.demand,
            s.load,
            s.vibration,
            MANUAL_PWM,
            ai_pwm,
            manual_error,
            ai_error,
            infer_us
        );
    }

    let mean_infer_us = infer_total_us / SAMPLES.len() as u128;
    println!(
        "REDCOLA_STARRY_CONTROL_SUMMARY manual_abs_error={} ai_abs_error={} max_ai_error={} \
         mean_infer_us={}",
        manual_abs_error, ai_abs_error, max_ai_error, mean_infer_us
    );
    if ai_abs_error < manual_abs_error && max_ai_error <= 35 {
        println!(
            "REDCOLA_STARRY_AI_CONTROL_PASS samples={} manual_abs_error={} ai_abs_error={} \
             mean_infer_us={}",
            SAMPLES.len(),
            manual_abs_error,
            ai_abs_error,
            mean_infer_us
        );
        println!("REDCOLA_STARRY_AI_DONE");
    } else {
        println!("REDCOLA_STARRY_AI_CONTROL_FAIL");
        process::exit(1);
    }
}
