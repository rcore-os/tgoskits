use std::{
    env,
    fs::File,
    io::{self, BufRead, BufReader},
    process::ExitCode,
};

use ivcproto::{
    so100::{AuthorizationGate, FixedId1Command, GateOutcome, parse_rtos_authorization},
    vision::VisionAction,
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("IVC-VISION-ACTUATOR-FAIL reason={error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut arguments = env::args().skip(1);
    let mode = arguments.next().ok_or_else(usage)?;
    let input = arguments.next().unwrap_or_else(|| "-".to_owned());
    if arguments.next().is_some() {
        return Err(usage());
    }
    if mode == "--execute" {
        return Err(concat!(
            "physical execution remains locked: the shared /usb@fc880000 topology and ",
            "supervised SO-100 device I/O have not passed their independent activation gates"
        )
        .to_owned());
    }
    if mode != "--dry-run" {
        return Err(usage());
    }

    println!(concat!(
        "VISION_ACTUATOR_BEGIN mode=dry-run motor_id=1 left_position=2042 ",
        "right_position=2074 stable_frames=3 physical_verified=0 device_io_attempted=0"
    ));
    let count = if input == "-" {
        let stdin = io::stdin();
        plan_authorizations(stdin.lock())?
    } else {
        let file = File::open(&input).map_err(|error| format!("open {input}: {error}"))?;
        plan_authorizations(BufReader::new(file))?
    };
    println!(
        concat!(
            "VISION_ACTUATOR_DONE authorizations={} physical_applied=0 ",
            "physical_verified=0 device_io_attempted=0"
        ),
        count
    );
    Ok(())
}

fn plan_authorizations(reader: impl BufRead) -> Result<usize, String> {
    let mut gate = AuthorizationGate::new();
    let mut count = 0usize;
    for (line_index, line) in reader.lines().enumerate() {
        let line_number = line_index + 1;
        let line = line.map_err(|error| format!("read line {line_number}: {error}"))?;
        let Some(authorization) = parse_rtos_authorization(&line)
            .map_err(|error| format!("authorization line {line_number}: {error}"))?
        else {
            continue;
        };
        let outcome = gate
            .observe(authorization)
            .map_err(|error| format!("authorization line {line_number}: {error}"))?;
        match outcome {
            GateOutcome::Pending {
                action,
                observed,
                required,
            } => println!(
                concat!(
                    "VISION_ACTUATOR_RECORD version=1 session_id={} sequence={} frame_id={} ",
                    "action={} gate=pending stable={}/{} target_position=none ",
                    "physical_applied=0 physical_verified=0 device_io_attempted=0"
                ),
                authorization.session_id,
                authorization.sequence,
                authorization.frame_id,
                action_name(action),
                observed,
                required,
            ),
            GateOutcome::Stable(command) => println!(
                concat!(
                    "VISION_ACTUATOR_RECORD version=1 session_id={} sequence={} frame_id={} ",
                    "action={} gate=stable target_position={} physical_applied=0 ",
                    "physical_verified=0 device_io_attempted=0"
                ),
                authorization.session_id,
                authorization.sequence,
                authorization.frame_id,
                action_name(authorization.action),
                command_target(command),
            ),
            GateOutcome::NoChange(action) => println!(
                concat!(
                    "VISION_ACTUATOR_RECORD version=1 session_id={} sequence={} frame_id={} ",
                    "action={} gate=no-change target_position={} physical_applied=0 ",
                    "physical_verified=0 device_io_attempted=0"
                ),
                authorization.session_id,
                authorization.sequence,
                authorization.frame_id,
                action_name(action),
                command_target(ivcproto::so100::command_for_action(action)),
            ),
        }
        count = count
            .checked_add(1)
            .ok_or_else(|| "authorization count overflow".to_owned())?;
    }
    if count == 0 {
        return Err("input contains no RTOS authorization records".to_owned());
    }
    Ok(count)
}

const fn action_name(action: VisionAction) -> &'static str {
    match action {
        VisionAction::Hold => "hold",
        VisionAction::SortLeft => "left",
        VisionAction::SortRight => "right",
        VisionAction::EmergencyStop => "emergency-stop",
    }
}

const fn command_target(command: FixedId1Command) -> &'static str {
    match command {
        FixedId1Command::Hold => "hold",
        FixedId1Command::MoveTo(2042) => "2042",
        FixedId1Command::MoveTo(2074) => "2074",
        FixedId1Command::MoveTo(_) => "invalid",
        FixedId1Command::EmergencyStop => "emergency-stop",
    }
}

fn usage() -> String {
    "usage: ivc-vision-actuator <--dry-run|--execute> [controller-log|-]".to_owned()
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::plan_authorizations;

    #[test]
    fn dry_run_accepts_a_stable_three_frame_transition() {
        let input = (1..=3)
            .map(|sequence| {
                format!(
                    concat!(
                        "VISION_RTOS_AUTH_RECORD version=1 session_id=7 sequence={} ",
                        "frame_id={} requested_action=right authorized_action=right ",
                        "state=applied retries=0\n"
                    ),
                    sequence,
                    sequence + 10
                )
            })
            .collect::<String>();

        assert_eq!(plan_authorizations(Cursor::new(input)), Ok(3));
    }
}
