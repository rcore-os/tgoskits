use std::{env, fs, string::String, vec::Vec};

const DIR: &str = "/arceos-test-suit";
const FILE: &str = "/arceos-test-suit/basic.txt";
const SUBDIR: &str = "/arceos-test-suit/subdir";
const NESTED_FILE: &str = "/arceos-test-suit/subdir/nested.txt";
const CONTENT: &str = "arceos-test-suit fs smoke\n";
const NESTED_CONTENT: &str = "nested fs content\n";

pub fn run() -> crate::TestResult {
    let original_dir = env::current_dir().expect("failed to read current dir before fs test");

    let _ = fs::remove_file(NESTED_FILE);
    let _ = fs::remove_dir(SUBDIR);
    let _ = fs::remove_file(FILE);
    let _ = fs::remove_dir(DIR);

    fs::create_dir(DIR).expect("failed to create fs smoke directory");
    fs::write(FILE, CONTENT.as_bytes()).expect("failed to write fs smoke file");
    let text = fs::read_to_string(FILE).expect("failed to read fs smoke file");
    assert_eq!(text, CONTENT);
    assert!(fs::metadata(FILE).expect("missing fs smoke file").is_file());

    fs::create_dir(SUBDIR).expect("failed to create fs nested directory");
    fs::write(NESTED_FILE, NESTED_CONTENT.as_bytes()).expect("failed to write nested fs file");
    assert_eq!(
        fs::read(NESTED_FILE).expect("failed to read nested fs file"),
        NESTED_CONTENT.as_bytes()
    );

    let entries = sorted_dir_entries(DIR);
    assert!(
        entries.iter().any(|entry| entry == "basic.txt"),
        "fs read_dir did not return basic.txt: {entries:?}"
    );
    assert!(
        entries.iter().any(|entry| entry == "subdir"),
        "fs read_dir did not return subdir: {entries:?}"
    );

    env::set_current_dir(DIR).expect("failed to change into fs smoke directory");
    let current_dir = env::current_dir().expect("failed to read changed current dir");
    assert_eq!(normalize_dir_path(&current_dir), DIR);
    assert_eq!(
        fs::read_to_string("basic.txt").expect("failed to read relative fs smoke file"),
        CONTENT
    );
    env::set_current_dir(&original_dir).expect("failed to restore current dir after fs test");

    fs::remove_file(FILE).expect("failed to remove fs smoke file");
    fs::remove_file(NESTED_FILE).expect("failed to remove nested fs smoke file");
    fs::remove_dir(SUBDIR).expect("failed to remove nested fs smoke directory");
    fs::remove_dir(DIR).expect("failed to remove fs smoke directory");
    Ok(())
}

fn sorted_dir_entries(path: &str) -> Vec<String> {
    let mut entries = fs::read_dir(path)
        .expect("failed to read fs smoke directory")
        .map(|entry| {
            entry
                .expect("failed to read fs smoke directory entry")
                .file_name()
        })
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

fn normalize_dir_path(path: &str) -> &str {
    if path == "/" {
        path
    } else {
        path.trim_end_matches('/')
    }
}
