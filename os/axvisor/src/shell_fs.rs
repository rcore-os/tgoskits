// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::{
    ffi::OsString,
    fs::{self, File, FileTimes, Metadata, OpenOptions},
    io::{self, ErrorKind, Read, Write},
    os::unix::fs::MetadataExt,
    path::Path,
    string::{String, ToString},
    time::SystemTime,
    vec::Vec,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CopyMode {
    File,
    Recursive,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RemoveOptions {
    pub directory: bool,
    pub recursive: bool,
    pub force: bool,
}

pub fn collect_directory_entry_names<I>(entries: I, show_all: bool) -> io::Result<Vec<OsString>>
where
    I: IntoIterator<Item = io::Result<OsString>>,
{
    let mut names = entries
        .into_iter()
        .filter(|entry| {
            entry.as_ref().map_or(true, |name| {
                show_all || !name.to_string_lossy().starts_with('.')
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    names.sort();
    Ok(names)
}

pub fn remove_path(path: &str, options: RemoveOptions) -> io::Result<()> {
    match metadata_for_remove(path) {
        Ok(metadata) if metadata.is_dir() => {
            if options.recursive {
                remove_dir_recursive(path)
            } else if options.directory {
                fs::remove_dir(path)
            } else {
                Err(ErrorKind::Unsupported.into())
            }
        }
        Ok(_) => fs::remove_file(path),
        Err(error) if ignore_remove_error(options.force, error.kind()) => Ok(()),
        Err(error) => Err(error),
    }
}

pub fn metadata_for_remove(path: &str) -> io::Result<Metadata> {
    fs::symlink_metadata(path)
}

pub const fn ignore_remove_error(force: bool, kind: ErrorKind) -> bool {
    force && matches!(kind, ErrorKind::NotFound)
}

pub fn move_file_or_dir(source: &str, destination: &str) -> io::Result<()> {
    match fs::rename(source, destination) {
        Ok(()) => Ok(()),
        Err(error) if copy_after_rename_failure(error.kind()) => {
            if fs::metadata(source)?.is_dir() {
                copy_dir_recursive(source, destination)?;
                remove_dir_recursive(source)?;
            } else {
                copy_file(source, destination)?;
                fs::remove_file(source)?;
            }
            Ok(())
        }
        Err(error) => Err(error),
    }
}

pub const fn copy_after_rename_failure(kind: ErrorKind) -> bool {
    matches!(kind, ErrorKind::CrossesDevices)
}

pub fn touch_file(path: &str) -> io::Result<()> {
    touch_file_at(path, SystemTime::now())
}

pub fn touch_file_at(path: &str, time: SystemTime) -> io::Result<()> {
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    file.set_times(FileTimes::new().set_accessed(time).set_modified(time))
}

pub fn copy_path(source: &str, destination: &str, mode: CopyMode) -> io::Result<()> {
    let source_metadata = fs::metadata(source)?;
    let destination = effective_destination(source, destination)?;

    if source_metadata.is_dir() {
        if mode == CopyMode::Recursive {
            ensure_recursive_destination_outside_source(source, &destination)?;
            copy_dir_recursive(source, &destination)
        } else {
            Err(ErrorKind::Unsupported.into())
        }
    } else {
        ensure_file_destination_differs_from_source(source, &source_metadata, &destination)?;
        copy_file(source, &destination)
    }
}

pub fn copy_operands(args: &[String]) -> io::Result<(&str, &str)> {
    match args {
        [source, destination] => Ok((source, destination)),
        _ => Err(io::Error::new(
            ErrorKind::InvalidInput,
            "expected exactly one source and one destination",
        )),
    }
}

fn effective_destination(source: &str, destination: &str) -> io::Result<String> {
    match fs::metadata(destination) {
        Ok(metadata) if metadata.is_dir() => {
            let source_name = path_basename(source)?;
            Ok(format!("{destination}/{source_name}"))
        }
        Ok(_) => Ok(destination.to_string()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(destination.to_string()),
        Err(error) => Err(error),
    }
}

pub fn ensure_recursive_destination_outside_source(
    source: &str,
    destination: &str,
) -> io::Result<()> {
    let source_components = absolute_path_components(source)?;
    let destination_components = absolute_path_components(destination)?;

    if destination_components.starts_with(&source_components) {
        return Err(copy_into_itself_error());
    }

    let source_metadata = fs::metadata(source)?;
    let mut ancestor = Some(Path::new(destination));
    while let Some(path) = ancestor {
        if path.as_os_str().is_empty() {
            break;
        }
        match fs::metadata(path) {
            Ok(metadata) if same_file(&source_metadata, &metadata) => {
                return Err(copy_into_itself_error());
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        ancestor = path.parent();
    }
    Ok(())
}

fn ensure_file_destination_differs_from_source(
    source: &str,
    source_metadata: &Metadata,
    destination: &str,
) -> io::Result<()> {
    let same_path = absolute_path_components(source)? == absolute_path_components(destination)?;
    let same_node = match fs::metadata(destination) {
        Ok(destination_metadata) => same_file(source_metadata, &destination_metadata),
        Err(error) if error.kind() == ErrorKind::NotFound => false,
        Err(error) => return Err(error),
    };

    if same_path || same_node {
        Err(io::Error::new(
            ErrorKind::InvalidInput,
            "source and destination are the same file",
        ))
    } else {
        Ok(())
    }
}

fn same_file(left: &Metadata, right: &Metadata) -> bool {
    // FAT currently reports this placeholder identity for every regular file.
    let identity_is_meaningful =
        (left.dev(), left.ino()) != (0, 1) && (right.dev(), right.ino()) != (0, 1);
    identity_is_meaningful && left.dev() == right.dev() && left.ino() == right.ino()
}

fn copy_into_itself_error() -> io::Error {
    io::Error::new(
        ErrorKind::InvalidInput,
        "cannot copy a directory into itself",
    )
}

fn absolute_path_components(path: &str) -> io::Result<Vec<String>> {
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        let current_dir = std::env::current_dir()?;
        format!("{}/{path}", current_dir.to_string_lossy())
    };
    let mut components = Vec::new();

    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop();
            }
            component => components.push(component.to_string()),
        }
    }
    Ok(components)
}

pub fn path_basename(path: &str) -> io::Result<&str> {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "path has no basename"))
}

fn copy_file(source: &str, destination: &str) -> io::Result<()> {
    let mut source_file = File::open(source)?;
    let mut destination_file = File::create(destination)?;
    let mut buffer = [0; 4096];

    loop {
        let bytes_read = source_file.read(&mut buffer)?;
        if bytes_read == 0 {
            return Ok(());
        }
        destination_file.write_all(&buffer[..bytes_read])?;
    }
}

fn copy_dir_recursive(source: &str, destination: &str) -> io::Result<()> {
    fs::create_dir(destination)?;

    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        let source_path = format!("{source}/{file_name}");
        let destination_path = format!("{destination}/{file_name}");

        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&source_path, &destination_path)?;
        } else {
            copy_file(&source_path, &destination_path)?;
        }
    }
    Ok(())
}

fn remove_dir_recursive(path: &str) -> io::Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let entry_name = entry.file_name();
        let entry_name = entry_name.to_string_lossy();
        let entry_path = format!("{path}/{entry_name}");

        if entry.file_type()?.is_dir() {
            remove_dir_recursive(&entry_path)?;
        } else {
            fs::remove_file(&entry_path)?;
        }
    }
    fs::remove_dir(path)
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        io::{self, ErrorKind},
    };

    use super::{
        collect_directory_entry_names, copy_after_rename_failure, copy_operands,
        ignore_remove_error,
    };

    #[test]
    fn move_falls_back_to_copy_only_across_devices() {
        assert!(copy_after_rename_failure(ErrorKind::CrossesDevices));
        assert!(!copy_after_rename_failure(ErrorKind::PermissionDenied));
        assert!(!copy_after_rename_failure(ErrorKind::AlreadyExists));
    }

    #[test]
    fn remove_force_ignores_only_not_found() {
        assert!(ignore_remove_error(true, ErrorKind::NotFound));
        assert!(!ignore_remove_error(true, ErrorKind::PermissionDenied));
        assert!(!ignore_remove_error(true, ErrorKind::Unsupported));
        assert!(!ignore_remove_error(false, ErrorKind::NotFound));
    }

    #[test]
    fn directory_collection_propagates_iteration_errors() {
        let entries = [
            Ok(OsString::from("visible")),
            Err(io::Error::from(ErrorKind::PermissionDenied)),
        ];

        let error = collect_directory_entry_names(entries, false)
            .expect_err("directory iteration error must be propagated");
        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
    }

    #[test]
    fn copy_requires_exactly_two_operands() {
        let source = "source".to_string();
        let destination = "destination".to_string();
        let extra = "extra".to_string();
        assert!(copy_operands(&[]).is_err());
        assert!(copy_operands(core::slice::from_ref(&source)).is_err());
        assert!(copy_operands(&[source.clone(), destination.clone()]).is_ok());
        assert!(copy_operands(&[source, destination, extra]).is_err());
    }
}
