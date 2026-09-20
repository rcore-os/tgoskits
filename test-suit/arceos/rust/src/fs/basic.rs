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

    assert!(
        ax_std::fs::metadata("/")
            .expect("missing root metadata")
            .is_dir()
    );
    // Exercise the mini-std facade as well as the libc-backed standard library.
    ax_std::fs::create_dir_all(SUBDIR).expect("failed to recursively create fs directories");
    ax_std::fs::create_dir_all(SUBDIR).expect("existing directory must be accepted");
    ax_std::fs::create_dir_all("").expect("empty path must be accepted");
    assert!(
        ax_std::fs::metadata(DIR)
            .expect("missing directory metadata")
            .is_dir()
    );
    fs::write(FILE, CONTENT.as_bytes()).expect("failed to write fs smoke file");
    let text = fs::read_to_string(FILE).expect("failed to read fs smoke file");
    assert_eq!(text, CONTENT);
    assert!(fs::metadata(FILE).expect("missing fs smoke file").is_file());

    assert!(
        ax_std::fs::metadata(FILE)
            .expect("missing file metadata")
            .is_file()
    );
    assert!(ax_std::fs::create_dir_all(FILE).is_err());
    assert!(ax_std::fs::create_dir_all("/arceos-test-suit/basic.txt/child").is_err());
    assert_eq!(fs::read_to_string(FILE).unwrap(), CONTENT);
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
    assert_eq!(current_dir, std::path::Path::new(DIR));
    assert_eq!(
        fs::read_to_string("basic.txt").expect("failed to read relative fs smoke file"),
        CONTENT
    );
    ax_std::fs::create_dir_all("subdir//nested/../leaf/")
        .expect("failed to create relative directories");
    assert!(ax_std::fs::metadata("subdir/leaf").unwrap().is_dir());
    ax_std::fs::remove_dir("subdir/leaf").unwrap();
    ax_std::fs::remove_dir("subdir/nested").unwrap();
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
                .into_string()
                .expect("test entry name must be UTF-8")
        })
        .collect::<Vec<_>>();
    entries.sort();
    entries
}
