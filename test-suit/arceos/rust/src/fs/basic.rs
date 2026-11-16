use std::{env, fs, io, mem, ptr, string::String, vec::Vec};

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
    verify_fstatat_empty_path_and_cwd_contracts()?;
    verify_renameat_tracks_moved_directory_fd()?;

    env::set_current_dir(&original_dir).expect("failed to restore current dir after fs test");

    fs::remove_file(FILE).expect("failed to remove fs smoke file");
    fs::remove_file(NESTED_FILE).expect("failed to remove nested fs smoke file");
    fs::remove_dir(SUBDIR).expect("failed to remove nested fs smoke directory");
    fs::remove_dir(DIR).expect("failed to remove fs smoke directory");
    Ok(())
}

fn verify_fstatat_empty_path_and_cwd_contracts() -> crate::TestResult {
    let file_fd = unsafe { libc::open(c"/arceos-test-suit/basic.txt".as_ptr(), libc::O_RDONLY) };
    assert!(
        file_fd >= 0,
        "failed to open {FILE}: {:?}",
        io::Error::last_os_error()
    );

    let mut st_null: libc::stat = unsafe { mem::zeroed() };
    let ret = unsafe { libc::fstatat(file_fd, ptr::null(), &mut st_null, libc::AT_EMPTY_PATH) };
    assert_eq!(
        ret,
        0,
        "fstatat(fd, NULL, AT_EMPTY_PATH) failed: {:?}",
        io::Error::last_os_error()
    );
    assert_eq!(st_null.st_mode & libc::S_IFMT, libc::S_IFREG);
    assert_eq!(st_null.st_size, CONTENT.len() as _);

    let mut st_err: libc::stat = unsafe { mem::zeroed() };
    let ret = unsafe { libc::fstatat(file_fd, ptr::null(), &mut st_err, 0) };
    assert_eq!(ret, -1);
    assert_eq!(
        io::Error::last_os_error().raw_os_error(),
        Some(libc::EFAULT)
    );
    assert_eq!(unsafe { libc::close(file_fd) }, 0);

    let mut st_cwd: libc::stat = unsafe { mem::zeroed() };
    let ret = unsafe {
        libc::fstatat(
            libc::AT_FDCWD,
            c"".as_ptr(),
            &mut st_cwd,
            libc::AT_EMPTY_PATH,
        )
    };
    assert_eq!(
        ret,
        0,
        "fstatat(AT_FDCWD, \"\", AT_EMPTY_PATH) failed: {:?}",
        io::Error::last_os_error()
    );
    assert_eq!(st_cwd.st_mode & libc::S_IFMT, libc::S_IFDIR);

    let mut st_cwd_null: libc::stat = unsafe { mem::zeroed() };
    let ret = unsafe {
        libc::fstatat(
            libc::AT_FDCWD,
            ptr::null(),
            &mut st_cwd_null,
            libc::AT_EMPTY_PATH,
        )
    };
    assert_eq!(
        ret,
        0,
        "fstatat(AT_FDCWD, NULL, AT_EMPTY_PATH) failed: {:?}",
        io::Error::last_os_error()
    );
    assert_eq!(st_cwd_null.st_mode & libc::S_IFMT, libc::S_IFDIR);

    let ret = unsafe { libc::fstatat(libc::AT_FDCWD, c"".as_ptr(), &mut st_err, 0) };
    assert_eq!(ret, -1);
    assert_eq!(
        io::Error::last_os_error().raw_os_error(),
        Some(libc::ENOENT)
    );
    Ok(())
}

fn verify_renameat_tracks_moved_directory_fd() -> crate::TestResult {
    const SRC_DIR: &str = "/arceos-test-suit/rename_src";
    const MOVED_DIR: &str = "/arceos-test-suit/rename_moved";
    const SRC_ITEM: &str = "/arceos-test-suit/rename_src/item.txt";
    const DEST_FILE: &str = "/arceos-test-suit/dest.txt";
    const BACK_FILE: &str = "/arceos-test-suit/back.txt";
    const MOVED_ITEM: &str = "/arceos-test-suit/rename_moved/item.txt";
    const RETURNED_FILE: &str = "/arceos-test-suit/rename_moved/returned.txt";
    const ITEM_CONTENT: &str = "moved-dir-entry\n";

    let _ = fs::remove_file(DEST_FILE);
    let _ = fs::remove_file(BACK_FILE);
    let _ = fs::remove_file(RETURNED_FILE);
    let _ = fs::remove_file(MOVED_ITEM);
    let _ = fs::remove_file(SRC_ITEM);
    let _ = fs::remove_dir(MOVED_DIR);
    let _ = fs::remove_dir(SRC_DIR);

    fs::create_dir(SRC_DIR).expect("failed to create rename_src");
    fs::write(SRC_ITEM, ITEM_CONTENT.as_bytes()).expect("failed to write source item");

    // Rust std now retains a directory handle for DirEntry metadata on musl.
    // Keep an entry after its iterator closes and its original name is moved.
    let mut entries = fs::read_dir(SRC_DIR).expect("failed to read rename_src");
    let entry = entries.next().expect("missing source entry").unwrap();
    assert_eq!(entry.file_name(), "item.txt");
    drop(entries);

    let dir_fd = unsafe {
        libc::open(
            c"/arceos-test-suit/rename_src".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY,
        )
    };
    assert!(
        dir_fd >= 0,
        "failed to open rename_src dirfd: {:?}",
        io::Error::last_os_error()
    );

    fs::rename(SRC_DIR, MOVED_DIR).expect("failed to rename open directory");
    let metadata = entry
        .metadata()
        .expect("std DirEntry lost its renamed directory handle");
    assert!(metadata.is_file());
    assert_eq!(metadata.len(), ITEM_CONTENT.len() as u64);
    assert!(entry.file_type().unwrap().is_file());
    drop(entry);

    let mut st_dir: libc::stat = unsafe { mem::zeroed() };
    let ret = unsafe { libc::fstatat(dir_fd, ptr::null(), &mut st_dir, libc::AT_EMPTY_PATH) };
    assert_eq!(
        ret,
        0,
        "fstatat(dir_fd, NULL, AT_EMPTY_PATH) failed after rename: {:?}",
        io::Error::last_os_error()
    );
    assert_eq!(st_dir.st_mode & libc::S_IFMT, libc::S_IFDIR);

    let ret = unsafe {
        libc::renameat(
            dir_fd,
            c"item.txt".as_ptr(),
            libc::AT_FDCWD,
            c"/arceos-test-suit/dest.txt".as_ptr(),
        )
    };
    assert_eq!(
        ret,
        0,
        "renameat(dir_fd, \"item.txt\", AT_FDCWD, dest) failed after parent rename: {:?}",
        io::Error::last_os_error()
    );
    assert_eq!(fs::read_to_string(DEST_FILE).unwrap(), ITEM_CONTENT);
    assert!(fs::metadata(MOVED_ITEM).is_err());

    fs::write(BACK_FILE, ITEM_CONTENT.as_bytes()).expect("failed to write back.txt");
    let ret = unsafe {
        libc::renameat(
            libc::AT_FDCWD,
            c"/arceos-test-suit/back.txt".as_ptr(),
            dir_fd,
            c"returned.txt".as_ptr(),
        )
    };
    assert_eq!(
        ret,
        0,
        "renameat(AT_FDCWD, back.txt, dir_fd, \"returned.txt\") failed: {:?}",
        io::Error::last_os_error()
    );

    let mut st_rel: libc::stat = unsafe { mem::zeroed() };
    let ret = unsafe { libc::fstatat(dir_fd, c"returned.txt".as_ptr(), &mut st_rel, 0) };
    assert_eq!(ret, 0);
    assert_eq!(st_rel.st_size, ITEM_CONTENT.len() as _);

    assert_eq!(unsafe { libc::close(dir_fd) }, 0);
    fs::remove_file(DEST_FILE).unwrap();
    fs::remove_file(RETURNED_FILE).unwrap();
    fs::remove_dir(MOVED_DIR).unwrap();
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
