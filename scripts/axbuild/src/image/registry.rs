use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context, anyhow};
use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};

use super::spec::ImageSpecRef;
use crate::support::download::fetch_text;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageEntry {
    pub name: String,
    pub version: String,
    pub released_at: Option<DateTime<Utc>>,
    pub description: String,
    pub sha256: String,
    pub arch: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageRegistry {
    pub images: Vec<ImageEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IncludeEntry {
    url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RawRegistry {
    #[serde(default)]
    includes: Vec<IncludeEntry>,
    #[serde(default)]
    images: Vec<ImageEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrySource {
    pub url: String,
    pub kind: &'static str,
}

fn parse_registry_version(url: &str) -> Option<(u64, u64, u64)> {
    let file_name = url.rsplit('/').next()?;
    let version = file_name.strip_prefix('v')?.strip_suffix(".toml")?;
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

fn preferred_include(includes: &[IncludeEntry]) -> Option<&IncludeEntry> {
    let mut best: Option<(&IncludeEntry, (u64, u64, u64))> = None;
    for include in includes {
        let Some(version) = parse_registry_version(&include.url) else {
            continue;
        };
        if best.is_none_or(|(_, best_version)| version > best_version) {
            best = Some((include, version));
        }
    }

    best.map(|(include, _)| include).or_else(|| includes.last())
}

impl ImageRegistry {
    pub async fn fetch_with_includes(
        client: &reqwest::Client,
        url: &str,
    ) -> anyhow::Result<ImageRegistry> {
        use std::collections::{HashSet, VecDeque};

        let mut all_sources = Vec::new();
        let mut queue = VecDeque::from([url.to_string()]);
        let mut seen = HashSet::new();

        while let Some(current_url) = queue.pop_front() {
            if !seen.insert(current_url.clone()) {
                continue;
            }

            let body = fetch_text(client, &current_url).await?;
            let raw: RawRegistry = toml::from_str(&body)
                .map_err(|e| anyhow!("Invalid registry format at {}: {e}", current_url))?;

            all_sources.push(raw.images);
            for include in raw.includes {
                queue.push_back(include.url);
            }
        }

        Ok(ImageRegistry {
            images: merge_entries(all_sources),
        })
    }

    pub async fn resolve_bootstrap_source(
        client: &reqwest::Client,
        default_url: &str,
        fallback_url: &str,
    ) -> anyhow::Result<RegistrySource> {
        match fetch_text(client, default_url).await {
            Ok(body) => {
                let raw: RawRegistry = toml::from_str(&body)
                    .map_err(|e| anyhow!("Invalid registry format at {}: {e}", default_url))?;
                if let Some(include) = preferred_include(&raw.includes) {
                    Ok(RegistrySource {
                        url: include.url.clone(),
                        kind: "included registry from default.toml",
                    })
                } else {
                    Ok(RegistrySource {
                        url: default_url.to_string(),
                        kind: "default registry",
                    })
                }
            }
            Err(default_err) => {
                fetch_text(client, fallback_url).await.with_context(|| {
                    format!(
                        "failed to fetch default registry {default_url} and fallback registry \
                         {fallback_url}"
                    )
                })?;
                eprintln!("warning: failed to fetch default registry {default_url}: {default_err}");
                Ok(RegistrySource {
                    url: fallback_url.to_string(),
                    kind: "fallback registry",
                })
            }
        }
    }

    pub fn load_from_file(path: &Path) -> anyhow::Result<ImageRegistry> {
        let s = fs::read_to_string(path)
            .map_err(|e| anyhow!("Failed to read image registry from {}: {e}", path.display()))?;
        toml::from_str(&s).map_err(|e| anyhow!("Invalid image list format: {e}"))
    }

    pub fn print(&self, verbose: bool, pattern: Option<&str>) {
        print!("{}", self.render_table(verbose, pattern));
    }

    pub fn render_table(&self, verbose: bool, pattern: Option<&str>) -> String {
        let entries = self.filtered_entries(pattern);
        if verbose {
            self.render_verbose(&entries)
        } else {
            self.render_merged(&entries)
        }
    }

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

    fn render_verbose(&self, entries: &[&ImageEntry]) -> String {
        let rows: Vec<TableRow> = entries
            .iter()
            .map(|image| TableRow {
                name: image.name.clone(),
                version: image.version.clone(),
                arch: image.arch.clone(),
                description: image.description.clone(),
            })
            .collect();
        render_rows(&rows)
    }

    fn render_merged(&self, entries: &[&ImageEntry]) -> String {
        let by_name: BTreeMap<&str, Vec<&ImageEntry>> =
            entries.iter().fold(BTreeMap::new(), |mut m, e| {
                m.entry(e.name.as_str()).or_default().push(*e);
                m
            });
        let rows: Vec<TableRow> = by_name
            .into_iter()
            .map(|(name, vers)| {
                let first = vers.first().expect("non-empty grouped entries");
                let version = if vers.len() == 1 {
                    "1 version".to_string()
                } else {
                    format!("{} versions", vers.len())
                };
                TableRow {
                    name: name.to_string(),
                    version,
                    arch: first.arch.clone(),
                    description: first.description.clone(),
                }
            })
            .collect();
        render_rows(&rows)
    }

    pub fn find(&self, spec: ImageSpecRef<'_>) -> Option<&ImageEntry> {
        match spec.version {
            Some(version) => self
                .images
                .iter()
                .find(|entry| entry.name == spec.name && entry.version == version),
            None => self
                .images
                .iter()
                .filter(|entry| entry.name == spec.name)
                .max_by(|a, b| a.released_at.cmp(&b.released_at)),
        }
    }
}

struct TableRow {
    name: String,
    version: String,
    arch: String,
    description: String,
}

fn render_rows(rows: &[TableRow]) -> String {
    let name_width = column_width("Name", rows.iter().map(|row| row.name.as_str()));
    let version_width = column_width("Version", rows.iter().map(|row| row.version.as_str()));
    let arch_width = column_width("Architecture", rows.iter().map(|row| row.arch.as_str()));
    let description_width = column_width(
        "Description",
        rows.iter().map(|row| row.description.as_str()),
    );
    let total_width = name_width + version_width + arch_width + description_width + 6;

    let mut out = String::new();
    push_table_row(
        &mut out,
        ["Name", "Version", "Architecture", "Description"],
        [name_width, version_width, arch_width, description_width],
    );
    out.push_str(&format!("{}\n", "-".repeat(total_width)));
    for row in rows {
        push_table_row(
            &mut out,
            [&row.name, &row.version, &row.arch, &row.description],
            [name_width, version_width, arch_width, description_width],
        );
    }
    out
}

fn column_width<'a>(header: &str, values: impl Iterator<Item = &'a str>) -> usize {
    values
        .map(str::len)
        .chain(std::iter::once(header.len()))
        .max()
        .expect("header length is always present")
}

fn push_table_row(out: &mut String, cells: [&str; 4], widths: [usize; 4]) {
    out.push_str(&format!(
        "{:<name_width$}  {:<version_width$}  {:<arch_width$}  {:<description_width$}\n",
        cells[0],
        cells[1],
        cells[2],
        cells[3],
        name_width = widths[0],
        version_width = widths[1],
        arch_width = widths[2],
        description_width = widths[3],
    ));
}

fn merge_entries(sources: impl IntoIterator<Item = Vec<ImageEntry>>) -> Vec<ImageEntry> {
    use std::collections::HashMap;

    let mut by_key: HashMap<(String, String), ImageEntry> = HashMap::new();
    for entries in sources {
        for entry in entries {
            let key = (entry.name.clone(), entry.version.clone());
            by_key.entry(key).or_insert(entry);
        }
    }
    let mut out: Vec<ImageEntry> = by_key.into_values().collect();
    out.sort_by(|a, b| {
        (a.name.as_str(), a.version.as_str()).cmp(&(b.name.as_str(), b.version.as_str()))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> ImageRegistry {
        ImageRegistry {
            images: vec![
                ImageEntry {
                    name: "linux".to_string(),
                    version: "0.0.1".to_string(),
                    released_at: Some("2025-01-01T00:00:00Z".parse().unwrap()),
                    description: "Linux guest".to_string(),
                    sha256: "abc".to_string(),
                    arch: "aarch64".to_string(),
                    url: "https://example.com/linux-0.0.1.tar.gz".to_string(),
                },
                ImageEntry {
                    name: "linux".to_string(),
                    version: "0.0.2".to_string(),
                    released_at: Some("2025-01-02T00:00:00Z".parse().unwrap()),
                    description: "Linux guest".to_string(),
                    sha256: "def".to_string(),
                    arch: "aarch64".to_string(),
                    url: "https://example.com/linux-0.0.2.tar.gz".to_string(),
                },
                ImageEntry {
                    name: "nimbos".to_string(),
                    version: "0.0.1".to_string(),
                    released_at: Some("2025-01-03T00:00:00Z".parse().unwrap()),
                    description: "NimbOS guest".to_string(),
                    sha256: "ghi".to_string(),
                    arch: "x86_64".to_string(),
                    url: "https://example.com/nimbos-0.0.1.tar.gz".to_string(),
                },
            ],
        }
    }

    #[test]
    fn render_merged_groups_versions() {
        let table = registry().render_table(false, None);

        assert!(table.contains("linux"));
        assert!(table.contains("2 versions"));
        assert!(table.contains("nimbos"));
    }

    #[test]
    fn render_merged_aligns_long_names() {
        let images = ImageRegistry {
            images: vec![
                ImageEntry {
                    name: "initramfs-loongarch64-busybox.cpio.gz".to_string(),
                    version: "0.0.1".to_string(),
                    released_at: Some("2025-01-01T00:00:00Z".parse().unwrap()),
                    description: "BusyBox initramfs for loongarch64".to_string(),
                    sha256: "abc".to_string(),
                    arch: "loongarch64".to_string(),
                    url: "https://example.com/initramfs.tar.gz".to_string(),
                },
                ImageEntry {
                    name: "qemu-riscv64".to_string(),
                    version: "0.0.1".to_string(),
                    released_at: Some("2025-01-02T00:00:00Z".parse().unwrap()),
                    description: "Guest image bundle".to_string(),
                    sha256: "def".to_string(),
                    arch: "riscv64".to_string(),
                    url: "https://example.com/qemu.tar.gz".to_string(),
                },
            ],
        };

        let table = images.render_table(false, None);
        let version_column = table.lines().next().unwrap().find("Version").unwrap();
        for line in table.lines().skip(2) {
            assert_eq!(
                &line[version_column..version_column + "1 version".len()],
                "1 version"
            );
        }
    }

    #[test]
    fn render_verbose_shows_each_version() {
        let table = registry().render_table(true, None);

        assert!(table.contains("0.0.1"));
        assert!(table.contains("0.0.2"));
    }

    #[test]
    fn filtering_uses_regex_or_substring() {
        let table = registry().render_table(true, Some("^nim"));
        assert!(table.contains("nimbos"));
        assert!(!table.contains("linux"));

        let table = registry().render_table(true, Some("lin"));
        assert!(table.contains("linux"));
    }

    #[test]
    fn find_prefers_latest_when_version_omitted() {
        let images = registry();
        let entry = images.find(ImageSpecRef::parse("linux")).unwrap();
        assert_eq!(entry.version, "0.0.2");

        let exact = images.find(ImageSpecRef::parse("linux:0.0.1")).unwrap();
        assert_eq!(exact.version, "0.0.1");
    }

    #[test]
    fn preferred_include_uses_highest_registry_version() {
        let includes = vec![
            IncludeEntry {
                url: "https://example.com/registry/v0.0.20.toml".to_string(),
            },
            IncludeEntry {
                url: "https://example.com/registry/v0.0.22.toml".to_string(),
            },
            IncludeEntry {
                url: "https://example.com/registry/v0.0.25.toml".to_string(),
            },
        ];

        let include = preferred_include(&includes).unwrap();
        assert_eq!(include.url, "https://example.com/registry/v0.0.25.toml");
    }

    #[test]
    fn preferred_include_falls_back_to_last_when_versions_are_unparseable() {
        let includes = vec![
            IncludeEntry {
                url: "https://example.com/registry/alpha.toml".to_string(),
            },
            IncludeEntry {
                url: "https://example.com/registry/beta.toml".to_string(),
            },
        ];

        let include = preferred_include(&includes).unwrap();
        assert_eq!(include.url, "https://example.com/registry/beta.toml");
    }
}
