//! APK mirror region helpers shared by Starry prebuild and runtime rootfs flows.

use std::{fs, path::Path};

use anyhow::{Context, bail, ensure};

pub(crate) const STARRY_APK_REGION_VAR: &str = "STARRY_APK_REGION";
pub(crate) const STARRY_APK_MIRROR_VAR: &str = "STARRY_APK_MIRROR";
const CHINA_ALPINE_MIRROR: &str = "https://mirrors.cernet.edu.cn/alpine";
const LOCAL_ALPINE_MIRROR: &str = "http://192.168.1.2:3143/alpine";
const US_ALPINE_MIRROR: &str = "https://dl-cdn.alpinelinux.org/alpine";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApkRegion {
    China,
    Local,
    Us,
}

impl ApkRegion {
    pub(crate) fn canonical_name(self) -> &'static str {
        match self {
            Self::China => "china",
            Self::Local => "local",
            Self::Us => "us",
        }
    }

    fn mirror_base(self) -> &'static str {
        match self {
            Self::China => CHINA_ALPINE_MIRROR,
            Self::Local => LOCAL_ALPINE_MIRROR,
            Self::Us => US_ALPINE_MIRROR,
        }
    }
}

pub(crate) fn apk_region_from_env() -> anyhow::Result<ApkRegion> {
    let value = std::env::var(STARRY_APK_REGION_VAR).ok();
    parse_apk_region(value.as_deref())
}

pub(crate) fn parse_apk_region(value: Option<&str>) -> anyhow::Result<ApkRegion> {
    match value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
    {
        None => Ok(ApkRegion::Us),
        Some(value) if matches!(value.as_str(), "china" | "cn") => Ok(ApkRegion::China),
        Some(value) if matches!(value.as_str(), "local" | "lan") => Ok(ApkRegion::Local),
        Some(value) if matches!(value.as_str(), "us" | "usa") => Ok(ApkRegion::Us),
        Some(value) => bail!(
            "unsupported {STARRY_APK_REGION_VAR} `{value}`; supported values are: china, cn, us, \
             usa, local, lan"
        ),
    }
}

pub(crate) fn apk_mirror_from_env() -> anyhow::Result<String> {
    let region = apk_region_from_env()?;
    let mirror = std::env::var(STARRY_APK_MIRROR_VAR).ok();
    normalize_apk_mirror(mirror.as_deref().filter(|value| !value.trim().is_empty()))
        .map(|mirror| mirror.unwrap_or_else(|| region.mirror_base().to_string()))
}

pub(crate) fn rewrite_apk_repositories_for_env(staging_root: &Path) -> anyhow::Result<String> {
    let mirror = apk_mirror_from_env()?;
    rewrite_apk_repositories_for_mirror(staging_root, &mirror)?;
    Ok(mirror)
}

fn rewrite_apk_repositories_for_mirror(staging_root: &Path, mirror: &str) -> anyhow::Result<()> {
    let path = staging_root.join("etc/apk/repositories");
    let original =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let rewritten = rewrite_apk_repositories_content_with_mirror(&original, mirror);

    fs::write(&path, rewritten).with_context(|| format!("failed to write {}", path.display()))
}

pub(crate) fn rewrite_apk_repositories_content_with_mirror(original: &str, mirror: &str) -> String {
    let mirror = mirror.trim_end_matches('/');
    let ends_with_newline = original.ends_with('\n');
    let rewritten = original
        .lines()
        .map(|line| rewrite_apk_repository_line(line, mirror))
        .collect::<Vec<_>>()
        .join("\n");

    if ends_with_newline {
        rewritten + "\n"
    } else {
        rewritten
    }
}

fn rewrite_apk_repository_line(line: &str, mirror: &str) -> String {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return line.to_string();
    }

    let Some((_, suffix)) = trimmed.split_once("/alpine/") else {
        return line.to_string();
    };

    let leading_len = line.len() - line.trim_start().len();
    let trailing_len = line.len() - line.trim_end().len();
    let trailing = if trailing_len == 0 {
        ""
    } else {
        &line[line.len() - trailing_len..]
    };

    format!("{}{}/{}{}", &line[..leading_len], mirror, suffix, trailing)
}

fn normalize_apk_mirror(value: Option<&str>) -> anyhow::Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };

    let mirror = value.trim().trim_end_matches('/');
    ensure!(
        mirror.starts_with("http://") || mirror.starts_with("https://"),
        "{STARRY_APK_MIRROR_VAR} must start with http:// or https://"
    );
    ensure!(
        !mirror.chars().any(char::is_whitespace),
        "{STARRY_APK_MIRROR_VAR} must not contain whitespace"
    );
    let host = mirror
        .strip_prefix("http://")
        .or_else(|| mirror.strip_prefix("https://"))
        .unwrap_or_default();
    ensure!(
        !host.is_empty() && !host.starts_with('/'),
        "{STARRY_APK_MIRROR_VAR} must include a mirror host"
    );
    Ok(Some(mirror.to_string()))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn rewrite_apk_repositories_preserves_comments_blank_lines_and_other_urls() {
        let input = "\n# keep me\n  https://example.com/not-alpine/main  \nhttps://mirror.nyist.edu.cn/alpine/v3.23/main\n";

        assert_eq!(
            rewrite_apk_repositories_content_with_mirror(input, US_ALPINE_MIRROR),
            "\n# keep me\n  https://example.com/not-alpine/main  \nhttps://dl-cdn.alpinelinux.org/alpine/v3.23/main\n"
        );
    }

    #[test]
    fn rewrite_apk_repositories_for_region_updates_file() {
        let root = tempdir().unwrap();
        let repositories = root.path().join("etc/apk/repositories");
        fs::create_dir_all(repositories.parent().unwrap()).unwrap();
        fs::write(
            &repositories,
            "https://mirrors.cernet.edu.cn/alpine/v3.23/main\nhttps://mirrors.cernet.edu.cn/alpine/v3.23/community\n",
        )
        .unwrap();

        rewrite_apk_repositories_for_mirror(root.path(), US_ALPINE_MIRROR).unwrap();

        assert_eq!(
            fs::read_to_string(&repositories).unwrap(),
            "https://dl-cdn.alpinelinux.org/alpine/v3.23/main\nhttps://dl-cdn.alpinelinux.org/alpine/v3.23/community\n"
        );
    }

    #[test]
    fn local_region_rewrites_to_the_runner_lan_mirror() {
        let input = "https://mirrors.cernet.edu.cn/alpine/v3.23/main\n";

        assert_eq!(
            rewrite_apk_repositories_content_with_mirror(input, LOCAL_ALPINE_MIRROR),
            "http://192.168.1.2:3143/alpine/v3.23/main\n"
        );
    }

    #[test]
    fn custom_mirror_rewrites_repositories_and_removes_trailing_slash() {
        let input = "https://mirrors.cernet.edu.cn/alpine/v3.23/main\n";

        assert_eq!(
            rewrite_apk_repositories_content_with_mirror(input, "http://192.168.1.2:3143/alpine/",),
            "http://192.168.1.2:3143/alpine/v3.23/main\n"
        );
        assert_eq!(
            normalize_apk_mirror(Some("http://192.168.1.2:3143/alpine/")).unwrap(),
            Some("http://192.168.1.2:3143/alpine".to_string())
        );
    }

    #[test]
    fn custom_mirror_rejects_non_http_urls_and_whitespace() {
        assert!(normalize_apk_mirror(Some("ftp://mirror/alpine")).is_err());
        assert!(normalize_apk_mirror(Some("http://mirror/alpine path")).is_err());
        assert!(normalize_apk_mirror(Some("https://")).is_err());
    }
}
