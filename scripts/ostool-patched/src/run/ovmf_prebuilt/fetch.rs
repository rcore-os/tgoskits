use super::{Error, Source};
use indicatif::{ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Cursor, ErrorKind, Read};
use std::path::{Path, PathBuf};
use tar::Archive;
use ureq::Agent;

/// User-Agent header to send with download requests.
// const USER_AGENT: &str = "https://github.com/rust-osdev/ovmf-prebuilt";
const USER_AGENT: &str = "https://gitee.com/zr233/ovmf-prebuilt";

/// Maximum number of bytes to download (10 MiB).
const MAX_DOWNLOAD_SIZE_IN_BYTES: usize = 10 * 1024 * 1024;

/// Update the local cache. Does nothing if the cache is already up to date.
pub(crate) fn update_cache(source: Source, prebuilt_dir: &Path) -> Result<(), Error> {
    let hash_path = prebuilt_dir.join("sha256");

    // Check if the hash file already has the expected hash in it. If so, assume
    // that we've already got the correct prebuilt downloaded and unpacked.
    match fs::read_to_string(&hash_path) {
        Ok(current_hash) if current_hash == source.sha256 => return Ok(()),
        Ok(_) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(source) => {
            return Err(Error::HashRead {
                path: hash_path.clone(),
                source,
            });
        }
    }

    // let base_url = "https://github.com/rust-osdev/ovmf-prebuilt/releases/download";
    let base_url = "https://gitee.com/zr233/ovmf-prebuilt/releases/download";
    let url = format!(
        "{base_url}/{release}/{release}-bin.tar.xz",
        release = source.tag
    );

    let data = download_url(&url)?;

    // Validate the hash.
    let actual_hash: String = Sha256::digest(&data)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if actual_hash != source.sha256 {
        return Err(Error::HashMismatch {
            actual: actual_hash,
            expected: source.sha256.to_owned(),
        });
    }

    // Unpack the tarball.
    let decompressed = decompress(&data)?;

    // Clear out the existing prebuilt dir, if present.
    if let Err(source) = fs::remove_dir_all(prebuilt_dir)
        && source.kind() != ErrorKind::NotFound
    {
        return Err(Error::RemoveDir {
            path: prebuilt_dir.to_path_buf(),
            source,
        });
    }

    // Extract the files.
    extract(&decompressed, prebuilt_dir)?;

    // Write out the hash file. When we upgrade to a new release of
    // ovmf-prebuilt, the hash will no longer match, triggering a fresh
    // download.
    fs::write(&hash_path, actual_hash).map_err(|source| Error::HashWrite {
        path: hash_path.clone(),
        source,
    })?;

    Ok(())
}

/// Download `url` and return the raw data.
fn download_url(url: &str) -> Result<Vec<u8>, Error> {
    let config = Agent::config_builder().user_agent(USER_AGENT).build();
    let agent = Agent::new_with_config(config);

    // Download the file.
    info!("downloading {url}");
    let resp = agent
        .get(url)
        .call()
        .map_err(|err| Error::Request(Box::new(err)))?;

    // Get content length if available
    let content_length = resp
        .headers()
        .get("content-length")
        .and_then(|s| s.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());

    // Create progress bar
    let progress = if let Some(total) = content_length {
        let pb = ProgressBar::new(total);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{msg}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})")
                .unwrap()
                .progress_chars("#>-"),
        );
        pb.set_message(format!(
            "Downloading {}",
            url.split('/').next_back().unwrap_or("file")
        ));
        pb
    } else {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("{msg} {spinner:.green} [{elapsed_precise}] {bytes} ({bytes_per_sec})")
                .unwrap(),
        );
        pb.set_message(format!(
            "Downloading {}",
            url.split('/').next_back().unwrap_or("file")
        ));
        pb
    };

    let mut data = Vec::with_capacity(MAX_DOWNLOAD_SIZE_IN_BYTES);
    let mut reader = resp
        .into_body()
        .into_reader()
        // Limit the size of the download.
        .take(MAX_DOWNLOAD_SIZE_IN_BYTES.try_into().unwrap());

    // Read in chunks and update progress
    let mut buffer = [0u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                data.extend_from_slice(&buffer[..n]);
                progress.inc(n as u64);
            }
            Err(e) => {
                progress.finish_and_clear();
                return Err(Error::Download(e));
            }
        }
    }

    progress.finish_with_message(format!("Downloaded {} bytes", data.len()));
    info!("received {} bytes", data.len());

    Ok(data)
}

fn decompress(data: &[u8]) -> Result<Vec<u8>, Error> {
    info!("decompressing tarball");
    let mut decompressed = Vec::new();
    let mut compressed = Cursor::new(data);
    lzma_rs::xz_decompress(&mut compressed, &mut decompressed).map_err(Error::Decompress)?;
    Ok(decompressed)
}

/// Extract the tarball's files into `prebuilt_dir`.
///
/// `tarball_data` is raw decompressed tar data.
fn extract(tarball_data: &[u8], prebuilt_dir: &Path) -> Result<(), Error> {
    let cursor = Cursor::new(tarball_data);
    let mut archive = Archive::new(cursor);

    // Extract each file entry.
    for entry in archive.entries().map_err(Error::ArchiveEntries)? {
        let mut entry = entry.map_err(Error::ArchiveEntry)?;

        // Skip directories.
        if entry.size() == 0 {
            continue;
        }

        let path = entry.path().map_err(Error::ArchiveEntryPath)?;
        // Strip the leading directory, which is the release name.
        let path: PathBuf = path.components().skip(1).collect();

        let dir = path.parent().unwrap_or_else(|| Path::new(""));
        let dst_dir = prebuilt_dir.join(dir);
        let dst_path = prebuilt_dir.join(&path);
        info!("unpacking to {}", dst_path.display());
        fs::create_dir_all(&dst_dir).map_err(|source| Error::CreateDir {
            path: dst_dir.clone(),
            source,
        })?;
        entry.unpack(&dst_path).map_err(|source| Error::Unpack {
            path: dst_path.clone(),
            source,
        })?;
    }

    Ok(())
}
