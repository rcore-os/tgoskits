use std::{fs, path::PathBuf};

use ostool::board::{RunBoardOptions, config::BoardRunConfig};
use serde::Serialize;
use tempfile::tempdir;

use super::board_run_request;

#[derive(Serialize)]
struct LegacyBoardConfigFixture {
    board_type: String,
}

#[test]
fn session_files_round_trip_as_typed_toml() {
    let config = BoardRunConfig {
        board_type: "OrangePi-5-Plus".to_string(),
        session_files: vec![
            PathBuf::from("iperf-smoke.sh"),
            PathBuf::from("tools/network/probe.sh"),
        ],
        ..Default::default()
    };

    let encoded = toml::to_string(&config).unwrap();
    let decoded: BoardRunConfig = toml::from_str(&encoded).unwrap();

    assert_eq!(decoded, config);
}

#[test]
fn legacy_board_config_defaults_to_empty_session_files() {
    let fixture = LegacyBoardConfigFixture {
        board_type: "OrangePi-5-Plus".to_string(),
    };
    let encoded = toml::to_string(&fixture).unwrap();

    let decoded: BoardRunConfig = toml::from_str(&encoded).unwrap();

    assert!(decoded.session_files.is_empty());
}

#[test]
fn board_request_resolves_nested_files_from_config_directory() {
    let root = tempdir().unwrap();
    let case_dir = root.path().join("iperf-smoke");
    fs::create_dir_all(case_dir.join("tools/network")).unwrap();
    fs::write(case_dir.join("tools/network/probe.sh"), b"probe").unwrap();
    let config_path = case_dir.join("board-orangepi-5-plus.toml");
    let config = BoardRunConfig {
        board_type: "OrangePi-5-Plus".to_string(),
        session_files: vec![PathBuf::from("tools/network/probe.sh")],
        ..Default::default()
    };
    fs::write(&config_path, toml::to_string(&config).unwrap()).unwrap();

    board_run_request(&config_path, config, RunBoardOptions::default()).unwrap();
}

#[test]
fn board_request_rejects_invalid_duplicate_and_missing_files() {
    let root = tempdir().unwrap();
    let case_dir = root.path().join("iperf-smoke");
    fs::create_dir_all(&case_dir).unwrap();
    fs::write(case_dir.join("iperf-smoke.sh"), b"probe").unwrap();
    let config_path = case_dir.join("board-orangepi-5-plus.toml");

    for session_files in [
        vec![PathBuf::from("../escape.sh")],
        vec![case_dir.join("iperf-smoke.sh")],
        vec![
            PathBuf::from("iperf-smoke.sh"),
            PathBuf::from("iperf-smoke.sh"),
        ],
        vec![PathBuf::from("missing.sh")],
    ] {
        let config = BoardRunConfig {
            board_type: "OrangePi-5-Plus".to_string(),
            session_files,
            ..Default::default()
        };
        assert!(board_run_request(&config_path, config, RunBoardOptions::default()).is_err());
    }
}
