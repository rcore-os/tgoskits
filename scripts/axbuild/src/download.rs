use std::{path::Path, time::Duration};

use anyhow::Context;
use futures_util::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use tokio::{fs as tokio_fs, io::AsyncWriteExt};

pub(crate) fn http_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(60 * 30))
        .build()
        .map_err(|e| anyhow!("failed to create HTTP client: {e}"))
}

pub(crate) async fn fetch_text(client: &reqwest::Client, url: &str) -> anyhow::Result<String> {
    client
        .get(url)
        .send()
        .await
        .with_context(|| format!("failed to request {url}"))?
        .error_for_status()
        .with_context(|| format!("failed to fetch {url}"))?
        .text()
        .await
        .with_context(|| format!("failed to read response body from {url}"))
}

pub(crate) async fn download_to_path_with_progress(
    client: &reqwest::Client,
    url: &str,
    output_path: &Path,
) -> anyhow::Result<()> {
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("failed to request {url}"))?
        .error_for_status()
        .with_context(|| format!("failed to download {url}"))?;

    let total_size = response.content_length();
    let progress = new_progress_bar(total_size, output_path);
    let mut file = tokio_fs::File::create(output_path)
        .await
        .with_context(|| format!("failed to create {}", output_path.display()))?;
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| format!("failed while downloading {url}"))?;
        file.write_all(&chunk)
            .await
            .with_context(|| format!("failed to write {}", output_path.display()))?;
        progress.inc(chunk.len() as u64);
    }

    file.flush()
        .await
        .with_context(|| format!("failed to flush {}", output_path.display()))?;
    progress.finish_with_message(format!("downloaded {}", output_path.display()));
    Ok(())
}

pub(crate) fn new_progress_bar(total_size: Option<u64>, output_path: &Path) -> ProgressBar {
    match total_size {
        Some(total_size) => {
            let progress = ProgressBar::new(total_size);
            progress.set_style(
                ProgressStyle::with_template(
                    "{spinner:.green} {msg} [{bar:40.cyan/blue}] {bytes}/{total_bytes} \
                     ({bytes_per_sec}, ETA {eta})",
                )
                .expect("valid progress bar template")
                .progress_chars("##-"),
            );
            progress.set_message(format!("downloading {}", output_path.display()));
            progress
        }
        None => {
            let progress = ProgressBar::new_spinner();
            progress.set_message(format!("downloading {}", output_path.display()));
            progress.enable_steady_tick(std::time::Duration::from_millis(100));
            progress
        }
    }
}
