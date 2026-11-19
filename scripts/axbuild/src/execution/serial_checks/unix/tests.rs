use std::{io::BufRead, os::unix::net::UnixListener, sync::mpsc};

use super::*;

fn buffered_checks(text: &str) -> Checks {
    Checks {
        shared: Arc::new(Shared {
            state: Mutex::new({
                let mut state = State::default();
                state.append(text, Instant::now());
                state
            }),
            changed: Condvar::new(),
            writer: Mutex::new(None),
            cancelled: Arc::new(AtomicBool::new(false)),
        }),
        prefix: None,
        require_newline: false,
        active_failures: Vec::new(),
    }
}

#[test]
fn expired_serial_check_cannot_accept_a_late_buffered_marker() {
    let checks = buffered_checks("READY");
    let deadline = Instant::now() - Duration::from_secs(1);
    assert!(
        checks
            .wait(deadline, |text| Ok(text.find("READY").map(|i| i + 5)))
            .is_err()
    );
}

#[test]
fn failure_only_step_does_not_silently_ignore_its_failure_pattern() {
    let mut checks = buffered_checks("ERROR\n");
    let result = checks.run(
        &[ShellCheckStep {
            fail_regex: Some(vec!["ERROR".into()]),
            ..Default::default()
        }],
        Duration::from_secs(1),
    );
    assert!(result.is_err(), "failure-only checks must observe errors");
}

#[test]
fn serial_checks_preserve_commands_and_monitor_failures_after_completion() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("console.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let (release, wait) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut input = io::BufReader::new(stream.try_clone().unwrap());
        stream.write_all(b"\n\x1b[32mguest> \x1b[0m").unwrap();
        let mut command = String::new();
        input.read_line(&mut command).unwrap();
        assert_eq!(command, "history -c\n");
        stream
            .write_all(b"\r\x1b[2Kguest> history -c\r\nguest> ")
            .unwrap();
        command.clear();
        input.read_line(&mut command).unwrap();
        assert_eq!(command, "perform\n");
        stream
            .write_all(b"perform\r\nnoise\xff\xfe\nRESULT=42\r\nguest> ")
            .unwrap();
        wait.recv().unwrap();
        stream
            .write_all(b"\x1b[31mPANIC: late failure\x1b[0m\r\n")
            .unwrap();
    });
    let (monitor, mut checks) = Monitor::start(
        &socket,
        &[r"(?i)\bPANIC\b".into()],
        Arc::new(AtomicBool::new(false)),
        Duration::from_secs(2),
        io::sink(),
    )
    .unwrap();
    checks
        .run(
            &[
                ShellCheckStep {
                    shell_prefix: Some("guest>".into()),
                    shell_cmd: Some("history -c".into()),
                    ..Default::default()
                },
                ShellCheckStep {
                    shell_cmd: Some("perform".into()),
                    success_regex: Some(vec!["(?m)^RESULT=42$".into()]),
                    ..Default::default()
                },
            ],
            Duration::from_secs(2),
        )
        .unwrap();
    release.send(()).unwrap();
    let error = checks
        .wait(Instant::now() + Duration::from_secs(2), |_| Ok(None))
        .unwrap_err();
    assert!(error.to_string().contains("global fail_regex"), "{error:#}");
    assert!(monitor.finish().is_err());
    server.join().unwrap();
}

#[test]
fn serial_result_after_a_prefix_is_not_discarded_before_sending() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("console.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.write_all(b"guest>\nALREADY_STOPPED\n").unwrap();
        let mut input = io::BufReader::new(stream);
        let mut command = String::new();
        input.read_line(&mut command).unwrap();
        assert_eq!(command, "detach\n");
    });
    let (monitor, mut checks) = Monitor::start(
        &socket,
        &[],
        Arc::new(AtomicBool::new(false)),
        Duration::from_secs(2),
        io::sink(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut state = checks.shared.state.lock().unwrap();
    while !state.pending.ends_with("ALREADY_STOPPED\n") {
        assert!(Instant::now() < deadline);
        state = checks.shared.changed.wait_timeout(state, POLL).unwrap().0;
    }
    drop(state);
    checks
        .run(
            &[ShellCheckStep {
                shell_prefix: Some("guest>".into()),
                shell_cmd: Some("detach".into()),
                success_regex: Some(vec!["(?m)^ALREADY_STOPPED$".into()]),
                ..Default::default()
            }],
            Duration::from_secs(1),
        )
        .unwrap();
    monitor.finish().unwrap();
    server.join().unwrap();
}

#[test]
fn finish_drains_queued_serial_errors_after_the_last_check() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("console.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let (release, wait) = mpsc::channel();
    let (written, queued) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut input = io::BufReader::new(stream.try_clone().unwrap());
        stream.write_all(b"ready>").unwrap();
        let mut command = String::new();
        input.read_line(&mut command).unwrap();
        assert_eq!(command, "ping\n");
        stream.write_all(b"OK\n").unwrap();
        wait.recv().unwrap();
        let mut tail = vec![b'x'; 16384];
        tail.extend_from_slice(b"\nPANIC: queued after completion\n");
        stream.write_all(&tail).unwrap();
        written.send(()).unwrap();
    });
    let (monitor, mut checks) = Monitor::start(
        &socket,
        &["PANIC:".into()],
        Arc::new(AtomicBool::new(false)),
        Duration::from_secs(2),
        io::sink(),
    )
    .unwrap();
    checks
        .run(
            &[ShellCheckStep {
                shell_prefix: Some("ready>".into()),
                shell_cmd: Some("ping".into()),
                success_regex: Some(vec!["OK".into()]),
                ..Default::default()
            }],
            Duration::from_secs(2),
        )
        .unwrap();
    let state = checks.shared.state.lock().unwrap();
    release.send(()).unwrap();
    queued.recv().unwrap();
    monitor.stop.store(true, Ordering::Release);
    drop(state);
    assert!(
        monitor.finish().is_err(),
        "queued failure must not be lost during shutdown"
    );
    server.join().unwrap();
}

#[test]
fn serial_step_failure_wins_over_a_success_in_the_same_output() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("console.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.write_all(b"guest> ").unwrap();
        let mut input = io::BufReader::new(stream.try_clone().unwrap());
        let mut command = String::new();
        input.read_line(&mut command).unwrap();
        assert_eq!(command, "perform\n");
        stream.write_all(b"\r\nERROR\nRESULT=42\n").unwrap();
    });
    let (monitor, mut checks) = Monitor::start(
        &socket,
        &[],
        Arc::new(AtomicBool::new(false)),
        Duration::from_secs(2),
        io::sink(),
    )
    .unwrap();
    let error = checks
        .run(
            &[ShellCheckStep {
                shell_prefix: Some("guest>".into()),
                shell_cmd: Some("perform".into()),
                success_regex: Some(vec!["(?m)^RESULT=42$".into()]),
                fail_regex: Some(vec!["(?m)^ERROR$".into()]),
                ..Default::default()
            }],
            Duration::from_secs(2),
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("matched fail_regex"));
    monitor.finish().unwrap();
    server.join().unwrap();
}

#[test]
fn received_before_deadline_remains_valid_when_processed_later() {
    let checks = buffered_checks("");
    let deadline = Instant::now() - Duration::from_secs(1);
    {
        let mut state = checks.shared.state.lock().unwrap();
        state.append("EAR", deadline - Duration::from_secs(1));
        state.append("LY", deadline);
        state.append("LATE", deadline + Duration::from_nanos(1));
    }
    checks
        .wait(deadline, |text| Ok(text.find("EARLY").map(|i| i + 5)))
        .unwrap();
    assert!(
        checks
            .wait(deadline, |text| Ok(text.find("LATE").map(|i| i + 4)))
            .is_err()
    );
}

#[test]
fn failure_only_command_observes_errors_until_its_completion_prompt() {
    for fail in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("console.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(b"ready> ").unwrap();
            let mut input = io::BufReader::new(stream.try_clone().unwrap());
            let mut command = String::new();
            input.read_line(&mut command).unwrap();
            assert_eq!(command, "perform\n");
            stream
                .write_all(if fail {
                    b"\nERROR\nready> "
                } else {
                    b"\nready> "
                })
                .unwrap();
        });
        let (monitor, mut checks) = Monitor::start(
            &socket,
            &[],
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(2),
            io::sink(),
        )
        .unwrap();
        let result = checks.run(
            &[ShellCheckStep {
                shell_prefix: Some("ready>".into()),
                shell_cmd: Some("perform".into()),
                fail_regex: Some(vec!["ERROR".into()]),
                ..Default::default()
            }],
            Duration::from_secs(2),
        );
        assert_eq!(result.is_err(), fail, "{result:?}");
        monitor.finish().unwrap();
        server.join().unwrap();
    }
}
