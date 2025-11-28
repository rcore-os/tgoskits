use std::collections::BTreeMap;
#[cfg(feature = "fs")]
use std::fs::{self, File, FileType};
#[cfg(feature = "fs")]
use std::io::{self, Read, Write};
use std::println;
use std::string::{String, ToString};

use crate::shell::command::{CommandNode, FlagDef, ParsedCommand};

#[cfg(feature = "fs")]
macro_rules! print_err {
    ($cmd: literal, $msg: expr) => {
        println!("{}: {}", $cmd, $msg);
    };
    ($cmd: literal, $arg: expr, $err: expr) => {
        println!("{}: {}: {}", $cmd, $arg, $err);
    };
}

// Helper function: split whitespace
#[cfg(feature = "fs")]
fn split_whitespace(s: &str) -> (&str, &str) {
    let s = s.trim();
    if let Some(pos) = s.find(char::is_whitespace) {
        let (first, rest) = s.split_at(pos);
        (first, rest.trim())
    } else {
        (s, "")
    }
}

#[cfg(feature = "fs")]
fn do_ls(cmd: &ParsedCommand) {
    let args = &cmd.positional_args;
    let show_long = cmd.flags.get("long").unwrap_or(&false);
    let show_all = cmd.flags.get("all").unwrap_or(&false);

    let _current_dir = std::env::current_dir().unwrap();

    fn show_entry_info(path: &str, entry: &str, show_long: bool) -> io::Result<()> {
        if show_long {
            let metadata = fs::metadata(path)?;
            let size = metadata.len();
            let file_type = metadata.file_type();
            let file_type_char = file_type_to_char(file_type);
            let rwx = file_perm_to_rwx(metadata.permissions().mode());
            let rwx = unsafe { core::str::from_utf8_unchecked(&rwx) };
            println!("{}{} {:>8} {}", file_type_char, rwx, size, entry);
        } else {
            println!("{}", entry);
        }
        Ok(())
    }

    fn list_one(name: &str, print_name: bool, show_long: bool, show_all: bool) -> io::Result<()> {
        use std::vec::Vec;

        let is_dir = fs::metadata(name)?.is_dir();
        if !is_dir {
            return show_entry_info(name, name, show_long);
        }

        if print_name {
            println!("{}:", name);
        }

        let mut entries = fs::read_dir(name)?
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .filter(|name| show_all || !name.starts_with('.'))
            .collect::<Vec<_>>();
        entries.sort();

        for entry in entries {
            let path = format!("{name}/{entry}");
            if let Err(e) = show_entry_info(&path, &entry, show_long) {
                print_err!("ls", path, e);
            }
        }
        Ok(())
    }

    let targets = if args.is_empty() {
        vec![".".to_string()]
    } else {
        args.clone()
    };

    for (i, name) in targets.iter().enumerate() {
        if i > 0 {
            println!();
        }
        if let Err(e) = list_one(name, targets.len() > 1, *show_long, *show_all) {
            print_err!("ls", name, e);
        }
    }
}

#[cfg(feature = "fs")]
fn do_cat(cmd: &ParsedCommand) {
    let args = &cmd.positional_args;

    if args.is_empty() {
        print_err!("cat", "no file specified");
        return;
    }

    fn cat_one(fname: &str) -> io::Result<()> {
        let mut buf = [0; 1024];
        let mut file = File::open(fname)?;
        loop {
            let n = file.read(&mut buf)?;
            if n > 0 {
                io::stdout().write_all(&buf[..n])?;
            } else {
                return Ok(());
            }
        }
    }

    for fname in args {
        if let Err(e) = cat_one(fname) {
            print_err!("cat", fname, e);
        }
    }
}

#[cfg(feature = "fs")]
fn do_echo(cmd: &ParsedCommand) {
    let args = &cmd.positional_args;
    let no_newline = cmd.flags.get("no-newline").unwrap_or(&false);

    let args_str = args.join(" ");

    fn echo_file(fname: &str, text_list: &[&str]) -> io::Result<()> {
        let mut file = File::create(fname)?;
        for text in text_list {
            file.write_all(text.as_bytes())?;
        }
        Ok(())
    }

    if let Some(pos) = args_str.rfind('>') {
        let text_before = args_str[..pos].trim();
        let (fname, text_after) = split_whitespace(&args_str[pos + 1..]);
        if fname.is_empty() {
            print_err!("echo", "no file specified");
            return;
        };

        let text_list = [
            text_before,
            if !text_after.is_empty() { " " } else { "" },
            text_after,
            if !no_newline { "\n" } else { "" },
        ];
        if let Err(e) = echo_file(fname, &text_list) {
            print_err!("echo", fname, e);
        }
    } else if *no_newline {
        use std::print;

        print!("{}", args_str);
    } else {
        println!("{}", args_str);
    }
}

#[cfg(feature = "fs")]
fn do_mkdir(cmd: &ParsedCommand) {
    let args = &cmd.positional_args;
    let create_parents = cmd.flags.get("parents").unwrap_or(&false);

    if args.is_empty() {
        print_err!("mkdir", "missing operand");
        return;
    }

    fn mkdir_one(path: &str, create_parents: bool) -> io::Result<()> {
        if create_parents {
            fs::create_dir_all(path)
        } else {
            fs::create_dir(path)
        }
    }

    for path in args {
        if let Err(e) = mkdir_one(path, *create_parents) {
            print_err!("mkdir", format_args!("cannot create directory '{path}'"), e);
        }
    }
}

#[cfg(feature = "fs")]
fn do_rm(cmd: &ParsedCommand) {
    let args = &cmd.positional_args;
    let rm_dir = cmd.flags.get("dir").unwrap_or(&false);
    let recursive = cmd.flags.get("recursive").unwrap_or(&false);
    let force = cmd.flags.get("force").unwrap_or(&false);

    if args.is_empty() {
        print_err!("rm", "missing operand");
        return;
    }

    fn rm_one(path: &str, rm_dir: bool, recursive: bool, force: bool) -> io::Result<()> {
        let metadata = fs::metadata(path);

        if force && metadata.is_err() {
            return Ok(()); // Ignore non-existent files when in force mode
        }

        let metadata = metadata?;

        if metadata.is_dir() {
            if recursive {
                remove_dir_recursive(path, force)
            } else if rm_dir {
                fs::remove_dir(path)
            } else {
                Err(io::Error::Unsupported)
            }
        } else {
            fs::remove_file(path)
        }
    }

    for path in args {
        if let Err(e) = rm_one(path, *rm_dir, *recursive, *force)
            && !force
        {
            print_err!("rm", format_args!("cannot remove '{path}'"), e);
        }
    }
}

// Implementation of recursively deleting directories (manual recursion)
#[cfg(feature = "fs")]
fn remove_dir_recursive(path: &str, _force: bool) -> io::Result<()> {
    // Read directory contents
    let entries = fs::read_dir(path)?;

    // Remove all child items
    for entry_result in entries {
        let entry = entry_result?;
        let entry_path = format!("{}/{}", path, entry.file_name());
        let metadata = entry.file_type();

        if metadata.is_dir() {
            // Recursively delete subdirectory
            remove_dir_recursive(&entry_path, _force)?;
        } else {
            // Delete file
            fs::remove_file(&entry_path)?;
        }
    }

    // Delete empty directory
    fs::remove_dir(path)
}

#[cfg(feature = "fs")]
fn do_cd(cmd: &ParsedCommand) {
    let args = &cmd.positional_args;

    let target = if args.is_empty() {
        "/"
    } else if args.len() == 1 {
        &args[0]
    } else {
        print_err!("cd", "too many arguments");
        return;
    };

    if let Err(e) = std::env::set_current_dir(target) {
        print_err!("cd", target, e);
    }
}

#[cfg(feature = "fs")]
fn do_pwd(cmd: &ParsedCommand) {
    let _logical = cmd.flags.get("logical").unwrap_or(&false);

    let pwd = std::env::current_dir().unwrap();
    println!("{}", pwd);
}

fn do_uname(cmd: &ParsedCommand) {
    let show_all = cmd.flags.get("all").unwrap_or(&false);
    let show_kernel = cmd.flags.get("kernel-name").unwrap_or(&false);
    let show_arch = cmd.flags.get("machine").unwrap_or(&false);

    let arch = option_env!("AX_ARCH").unwrap_or("");
    let platform = option_env!("AX_PLATFORM").unwrap_or("");
    let smp = match option_env!("AX_SMP") {
        None | Some("1") => "",
        _ => " SMP",
    };
    let version = option_env!("CARGO_PKG_VERSION").unwrap_or("0.1.0");

    if *show_all {
        println!(
            "ArceOS {ver}{smp} {arch} {plat}",
            ver = version,
            smp = smp,
            arch = arch,
            plat = platform,
        );
    } else if *show_kernel {
        println!("ArceOS");
    } else if *show_arch {
        println!("{}", arch);
    } else {
        println!(
            "ArceOS {ver}{smp} {arch} {plat}",
            ver = version,
            smp = smp,
            arch = arch,
            plat = platform,
        );
    }
}

fn do_exit(cmd: &ParsedCommand) {
    let args = &cmd.positional_args;
    let exit_code = if args.is_empty() {
        0
    } else {
        args[0].parse::<i32>().unwrap_or(0)
    };

    println!("Bye~");
    std::process::exit(exit_code);
}

fn do_log(cmd: &ParsedCommand) {
    let args = &cmd.positional_args;

    if args.is_empty() {
        println!("Current log level: {:?}", log::max_level());
        return;
    }

    match args[0].as_str() {
        "on" | "enable" => log::set_max_level(log::LevelFilter::Info),
        "off" | "disable" => log::set_max_level(log::LevelFilter::Off),
        "error" => log::set_max_level(log::LevelFilter::Error),
        "warn" => log::set_max_level(log::LevelFilter::Warn),
        "info" => log::set_max_level(log::LevelFilter::Info),
        "debug" => log::set_max_level(log::LevelFilter::Debug),
        "trace" => log::set_max_level(log::LevelFilter::Trace),
        level => {
            println!("Unknown log level: {}", level);
            println!("Available levels: off, error, warn, info, debug, trace");
            return;
        }
    }
    println!("Log level set to: {:?}", log::max_level());
}

#[cfg(feature = "fs")]
fn do_mv(cmd: &ParsedCommand) {
    let args = &cmd.positional_args;

    if args.len() < 2 {
        print_err!("mv", "missing operand");
        return;
    }

    // If only two arguments, handle single file/dir move
    if args.len() == 2 {
        let source = &args[0];
        let dest = &args[1];

        // Check if destination exists and is a directory
        if let Ok(dest_meta) = fs::metadata(dest)
            && dest_meta.is_dir()
        {
            // Move source into destination directory
            let mut file_dir = fs::read_dir(dest).unwrap();
            let source_name = match file_dir.next() {
                Some(name) => {
                    let dir_name = name.expect("Failed to read directory");
                    let file = dir_name.file_name();
                    format!("{dest}/{file}")
                }
                None => {
                    print_err!("mv", format_args!("invalid source path '{source}'"));
                    return;
                }
            };
            let dest_path = format!("{dest}/{source_name}");
            if let Err(e) = move_file_or_dir(source, &dest_path) {
                print_err!(
                    "mv",
                    format_args!("cannot move '{source}' to '{dest_path}'"),
                    e
                );
            }
            return;
        }

        // Direct rename/move
        if let Err(e) = move_file_or_dir(source, dest) {
            print_err!("mv", format_args!("cannot move '{source}' to '{dest}'"), e);
        }
    } else {
        // Multiple sources - destination must be a directory
        let dest = &args[args.len() - 1];
        let sources = &args[..args.len() - 1];

        // Check if destination is a directory
        match fs::metadata(dest) {
            Ok(meta) if meta.is_dir() => {
                // Move each source into destination directory
                for source in sources {
                    let mut file_dir = fs::read_dir(source).unwrap();
                    let source_name = match file_dir.next() {
                        Some(name) => {
                            let dir_name = name.expect("Failed to read directory");
                            let file = dir_name.file_name();
                            format!("{dest}/{file}")
                        }
                        None => {
                            print_err!("mv", format_args!("invalid source path '{source}'"));
                            return;
                        }
                    };
                    let dest_path = format!("{dest}/{source_name}");
                    if let Err(e) = move_file_or_dir(source, &dest_path) {
                        print_err!(
                            "mv",
                            format_args!("cannot move '{source}' to '{dest_path}'"),
                            e
                        );
                    }
                }
            }
            Ok(_) => {
                print_err!("mv", format_args!("target '{dest}' is not a directory"));
            }
            Err(e) => {
                print_err!("mv", format_args!("cannot access '{dest}'"), e);
            }
        }
    }
}

// Helper function to move file or directory (handles cross-filesystem moves)
#[cfg(feature = "fs")]
fn move_file_or_dir(source: &str, dest: &str) -> io::Result<()> {
    // Try simple rename first (works within same filesystem)
    match fs::rename(source, dest) {
        Ok(()) => Ok(()),
        Err(_) => {
            // If rename fails, try copy + delete (for cross-filesystem moves)
            let src_meta = fs::metadata(source)?;

            if src_meta.is_dir() {
                // For directories, use recursive copy then remove
                copy_dir_recursive(source, dest)?;
                remove_dir_recursive(source, false)?;
            } else {
                // For files, copy then remove
                copy_file(source, dest)?;
                fs::remove_file(source)?;
            }
            Ok(())
        }
    }
}

#[cfg(feature = "fs")]
fn do_touch(cmd: &ParsedCommand) {
    let args = &cmd.positional_args;

    if args.is_empty() {
        print_err!("touch", "missing operand");
        return;
    }

    for filename in args {
        if let Err(e) = File::create(filename) {
            print_err!("touch", filename, e);
        }
    }
}

#[cfg(feature = "fs")]
fn do_cp(cmd: &ParsedCommand) {
    let args = &cmd.positional_args;
    let recursive = cmd.flags.get("recursive").unwrap_or(&false);

    if args.len() < 2 {
        print_err!("cp", "missing operand");
        return;
    }

    let source = &args[0];
    let dest = &args[1];

    // Check if source file/directory exists
    let src_metadata = match fs::metadata(source) {
        Ok(metadata) => metadata,
        Err(e) => {
            print_err!("cp", format_args!("cannot access '{source}'"), e);
            return;
        }
    };

    let result = if src_metadata.is_dir() {
        if *recursive {
            copy_dir_recursive(source, dest)
        } else {
            Err(io::Error::Unsupported)
        }
    } else {
        copy_file(source, dest)
    };

    if let Err(e) = result {
        print_err!("cp", format_args!("cannot copy '{source}' to '{dest}'"), e);
    }
}

// Manually implement file copy
#[cfg(feature = "fs")]
fn copy_file(src: &str, dst: &str) -> io::Result<()> {
    let mut src_file = File::open(src)?;
    let mut dst_file = File::create(dst)?;

    let mut buffer = [0; 4096];
    loop {
        let bytes_read = src_file.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        dst_file.write_all(&buffer[..bytes_read])?;
    }
    Ok(())
}

// Recursively copy directory
#[cfg(feature = "fs")]
fn copy_dir_recursive(src: &str, dst: &str) -> io::Result<()> {
    // Create target directory
    fs::create_dir(dst)?;

    // Read source directory contents
    let entries = fs::read_dir(src)?;

    for entry_result in entries {
        let entry = entry_result?;
        let file_name = entry.file_name();
        let src_path = format!("{src}/{file_name}");
        let dst_path = format!("{dst}/{file_name}");

        let metadata = entry.file_type();
        if metadata.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            copy_file(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

#[cfg(feature = "fs")]
fn file_type_to_char(ty: FileType) -> char {
    if ty.is_char_device() {
        'c'
    } else if ty.is_block_device() {
        'b'
    } else if ty.is_socket() {
        's'
    } else if ty.is_fifo() {
        'p'
    } else if ty.is_symlink() {
        'l'
    } else if ty.is_dir() {
        'd'
    } else if ty.is_file() {
        '-'
    } else {
        '?'
    }
}

#[rustfmt::skip]
#[cfg(feature = "fs")]
const fn file_perm_to_rwx(mode: u32) -> [u8; 9] {
    let mut perm = [b'-'; 9];
    macro_rules! set {
        ($bit:literal, $rwx:literal) => {
            if mode & (1 << $bit) != 0 {
                perm[8 - $bit] = $rwx
            }
        };
    }

    set!(2, b'r'); set!(1, b'w'); set!(0, b'x');
    set!(5, b'r'); set!(4, b'w'); set!(3, b'x');
    set!(8, b'r'); set!(7, b'w'); set!(6, b'x');
    perm
}

pub fn build_base_cmd(tree: &mut BTreeMap<String, CommandNode>) {
    // ls Command
    #[cfg(feature = "fs")]
    tree.insert(
        "ls".to_string(),
        CommandNode::new("List directory contents")
            .with_handler(do_ls)
            .with_usage("ls [OPTIONS] [DIRECTORY...]")
            .with_flag(
                FlagDef::new("long", "Use long listing format")
                    .with_short('l')
                    .with_long("long"),
            )
            .with_flag(
                FlagDef::new("all", "Show hidden files")
                    .with_short('a')
                    .with_long("all"),
            ),
    );

    // cat Command
    #[cfg(feature = "fs")]
    tree.insert(
        "cat".to_string(),
        CommandNode::new("Display file contents")
            .with_handler(do_cat)
            .with_usage("cat <FILE1> [FILE2...]"),
    );

    // echo Command
    #[cfg(feature = "fs")]
    tree.insert(
        "echo".to_string(),
        CommandNode::new("Display text")
            .with_handler(do_echo)
            .with_usage("echo [OPTIONS] [TEXT...]")
            .with_flag(
                FlagDef::new("no-newline", "Do not output trailing newline")
                    .with_short('n')
                    .with_long("no-newline"),
            ),
    );

    // mkdir Command
    #[cfg(feature = "fs")]
    tree.insert(
        "mkdir".to_string(),
        CommandNode::new("Create directories")
            .with_handler(do_mkdir)
            .with_usage("mkdir [OPTIONS] <DIRECTORY1> [DIRECTORY2...]")
            .with_flag(
                FlagDef::new("parents", "Create parent directories as needed")
                    .with_short('p')
                    .with_long("parents"),
            ),
    );

    // rm Command
    #[cfg(feature = "fs")]
    tree.insert(
        "rm".to_string(),
        CommandNode::new("Remove files and directories")
            .with_handler(do_rm)
            .with_usage("rm [OPTIONS] <FILE1> [FILE2...]")
            .with_flag(
                FlagDef::new("dir", "Remove empty directories")
                    .with_short('d')
                    .with_long("dir"),
            )
            .with_flag(
                FlagDef::new("recursive", "Remove directories recursively")
                    .with_short('r')
                    .with_long("recursive"),
            )
            .with_flag(
                FlagDef::new("force", "Force removal, ignore nonexistent files")
                    .with_short('f')
                    .with_long("force"),
            ),
    );

    // cd Command
    #[cfg(feature = "fs")]
    tree.insert(
        "cd".to_string(),
        CommandNode::new("Change directory")
            .with_handler(do_cd)
            .with_usage("cd [DIRECTORY]"),
    );

    // pwd Command
    #[cfg(feature = "fs")]
    tree.insert(
        "pwd".to_string(),
        CommandNode::new("Print working directory")
            .with_handler(do_pwd)
            .with_usage("pwd [OPTIONS]")
            .with_flag(
                FlagDef::new("logical", "Use logical path")
                    .with_short('L')
                    .with_long("logical"),
            ),
    );

    // uname Command
    tree.insert(
        "uname".to_string(),
        CommandNode::new("System information")
            .with_handler(do_uname)
            .with_usage("uname [OPTIONS]")
            .with_flag(
                FlagDef::new("all", "Show all information")
                    .with_short('a')
                    .with_long("all"),
            )
            .with_flag(
                FlagDef::new("kernel-name", "Show kernel name")
                    .with_short('s')
                    .with_long("kernel-name"),
            )
            .with_flag(
                FlagDef::new("machine", "Show machine architecture")
                    .with_short('m')
                    .with_long("machine"),
            ),
    );

    // exit Command
    tree.insert(
        "exit".to_string(),
        CommandNode::new("Exit the shell")
            .with_handler(do_exit)
            .with_usage("exit [EXIT_CODE]"),
    );

    // log Command
    tree.insert(
        "log".to_string(),
        CommandNode::new("Change log level")
            .with_handler(do_log)
            .with_usage("log [LEVEL]"),
    );

    // touch Command
    #[cfg(feature = "fs")]
    tree.insert(
        "touch".to_string(),
        CommandNode::new("Create empty files")
            .with_handler(do_touch)
            .with_usage("touch <FILE1> [FILE2...]"),
    );

    // cp Command
    #[cfg(feature = "fs")]
    tree.insert(
        "cp".to_string(),
        CommandNode::new("Copy files")
            .with_handler(do_cp)
            .with_usage("cp [OPTIONS] <SOURCE> <DEST>")
            .with_flag(
                FlagDef::new("recursive", "Copy directories recursively")
                    .with_short('r')
                    .with_long("recursive"),
            ),
    );

    // mv Command
    #[cfg(feature = "fs")]
    tree.insert(
        "mv".to_string(),
        CommandNode::new("Move/rename files")
            .with_handler(do_mv)
            .with_usage("mv <SOURCE> <DEST> | mv <SOURCE1> [SOURCE2...] <DIRECTORY>"),
    );
}
