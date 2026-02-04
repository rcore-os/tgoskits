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

//! Image registry data structures and parsing.
//!
//! Defines `ImageEntry` and `ImageRegistry` for the TOML-based image list format.

use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Result, anyhow};
use regex::Regex;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::download::download_to_string;
use super::spec::ImageSpecRef;

/// An image entry in the image list file (one row in the registry TOML).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageEntry {
    /// Unique image identifier (e.g. `qemu_x86_64_nimbos`, `evm3588_arceos`).
    pub name: String,
    /// Version of the image (required in registry config).
    pub version: String,
    /// Release timestamp (UTC). Optional. Entries without it sort earliest when resolving by name only.
    /// Serialized as RFC 3339 / ISO 8601 in TOML (e.g. `2026-01-06T03:10:51Z`).
    pub released_at: Option<DateTime<Utc>>,
    /// Short human-readable description of the image.
    pub description: String,
    /// SHA-256 checksum of the image archive (hex string).
    pub sha256: String,
    /// Target architecture (e.g. `x86_64`, `aarch64`).
    pub arch: String,
    /// URL to download the image archive (e.g. `.tar.gz`).
    pub url: String,
}

/// An image list contains a list of [`ImageEntry`]s (top-level structure of the registry TOML).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageRegistry {
    /// All image entries from the registry.
    pub images: Vec<ImageEntry>,
}

/// A single entry in the `[[includes]]` array of a registry TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncludeEntry {
    /// URL of another registry TOML to include and merge.
    pub url: String,
}

/// Raw registry as parsed from TOML: may contain `includes` and/or `images`.
/// Used when fetching from network; local saved registry has only `images`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawRegistry {
    /// Optional list of registry URLs to include and merge.
    #[serde(default)]
    pub includes: Vec<IncludeEntry>,
    /// Image entries (may be empty if registry only includes other URLs).
    #[serde(default)]
    pub images: Vec<ImageEntry>,
}

/// Merges multiple image entry lists into one, deduplicating by (name, version).
///
/// On conflict (same name+version, different other fields), prints a warning and keeps one entry.
///
/// # Arguments
///
/// * `sources` - Iterator of image entry vectors to merge
pub fn merge_entries(sources: impl IntoIterator<Item = Vec<ImageEntry>>) -> Vec<ImageEntry> {
    use std::collections::HashMap;
    let mut by_key: HashMap<(String, String), ImageEntry> = HashMap::new();
    for entries in sources {
        for entry in entries {
            let key = (entry.name.clone(), entry.version.clone());
            if let Some(existing) = by_key.get(&key) {
                if existing != &entry {
                    println!(
                        "Warning: conflict for image {} version {} (different description/sha256/arch/url); keeping existing entry.",
                        entry.name, entry.version
                    );
                }
                continue;
            }
            by_key.insert(key, entry);
        }
    }
    let mut out: Vec<ImageEntry> = by_key.into_values().collect();
    out.sort_by(|a, b| {
        (a.name.as_str(), a.version.as_str()).cmp(&(b.name.as_str(), b.version.as_str()))
    });
    out
}

impl ImageRegistry {
    /// Fetches a registry from `url`, resolving `[[includes]]`, and returns a merged registry.
    ///
    /// # Arguments
    ///
    /// * `url` - URL of the registry TOML to fetch
    pub async fn fetch_with_includes(url: &str) -> Result<ImageRegistry> {
        use std::collections::{HashSet, VecDeque};

        let mut all_sources: Vec<Vec<ImageEntry>> = Vec::new();
        let mut queue: VecDeque<String> = VecDeque::new();
        let mut seen: HashSet<String> = HashSet::new();
        queue.push_back(url.to_string());

        while let Some(current_url) = queue.pop_front() {
            if !seen.insert(current_url.clone()) {
                continue; // already fetched, skip
            }
            let body = download_to_string(&current_url).await?;
            let raw: RawRegistry = toml::from_str(&body)
                .map_err(|e| anyhow!("Invalid registry format at {}: {e}", current_url))?;

            all_sources.push(raw.images);

            for include in raw.includes {
                queue.push_back(include.url);
            }
        }

        let images = merge_entries(all_sources);
        Ok(ImageRegistry { images })
    }

    /// Loads the image registry from a TOML file.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the registry TOML file
    pub fn load_from_file(path: &Path) -> Result<ImageRegistry> {
        let s = fs::read_to_string(path)
            .map_err(|e| anyhow!("Failed to read image registry from {}: {e}", path.display()))?;
        toml::from_str(&s).map_err(|e| anyhow!("Invalid image list format: {e}"))
    }

    /// Prints the image list in a formatted table to stdout.
    ///
    /// # Arguments
    ///
    /// * `verbose` - If `true`, show each version separately; if `false`, merge same-name images and show version count
    /// * `pattern` - If `Some`, filter by name: try regex match first (partial), fallback to substring
    pub fn print(&self, verbose: bool, pattern: Option<&str>) {
        let entries = self.filtered_entries(pattern);
        if verbose {
            Self::print_verbose(&entries);
        } else {
            Self::print_merged(&entries);
        }
    }

    /// Filters entries by pattern: regex match (partial) or substring.
    fn filtered_entries<'a>(&'a self, pattern: Option<&str>) -> Vec<&'a ImageEntry> {
        let Some(pat) = pattern else {
            return self.images.iter().collect();
        };
        let re = Regex::new(pat).ok();
        self.images
            .iter()
            .filter(|e| match &re {
                Some(r) => r.is_match(&e.name),
                None => e.name.contains(pat),
            })
            .collect()
    }

    fn print_verbose(entries: &[&ImageEntry]) {
        println!(
            "{:<25} {:<12} {:<15} {:<50}",
            "Name", "Version", "Architecture", "Description"
        );
        println!("{}", "-".repeat(102));
        for image in entries {
            println!(
                "{:<25} {:<12} {:<15} {:<50}",
                image.name, image.version, image.arch, image.description
            );
        }
    }

    fn print_merged(entries: &[&ImageEntry]) {
        let by_name: BTreeMap<&str, Vec<&ImageEntry>> =
            entries.iter().fold(BTreeMap::new(), |mut m, e| {
                m.entry(e.name.as_str()).or_default().push(*e);
                m
            });
        println!(
            "{:<25} {:<12} {:<15} {:<50}",
            "Name", "Version", "Architecture", "Description"
        );
        println!("{}", "-".repeat(102));
        for (name, vers) in by_name {
            let n = vers.len();
            let version_str = if n == 1 {
                "1 version".to_string()
            } else {
                format!("{} versions", n)
            };
            let first = vers.first().unwrap();
            println!(
                "{:<25} {:<12} {:<15} {:<50}",
                name, version_str, first.arch, first.description
            );
        }
    }

    /// Returns an iterator over all image entries in the registry.
    pub fn iter(&self) -> impl Iterator<Item = &ImageEntry> {
        self.images.iter()
    }

    /// Looks up an image by spec (name and optional version).
    ///
    /// When version is `Some`, returns the exact match. When `None`, returns the entry with the
    /// latest `released_at`; entries without `released_at` sort earliest.
    ///
    /// # Arguments
    ///
    /// * `spec` - Image spec (name and optional version)
    pub fn find(&self, spec: ImageSpecRef<'_>) -> Option<&ImageEntry> {
        match spec.version {
            Some(v) => self.iter().find(|e| e.name == spec.name && e.version == v),
            None => self
                .iter()
                .filter(|e| e.name == spec.name)
                .max_by(|a, b| a.released_at.cmp(&b.released_at)),
        }
    }
}
