//! APK mirror region helpers shared by Starry prebuild and runtime rootfs flows.

use std::{fs, path::Path};

use anyhow::{Context, bail};

pub(crate) const STARRY_APK_REGION_VAR: &str = "STARRY_APK_REGION";
const CHINA_ALPINE_MIRROR: &str = "https://mirrors.cernet.edu.cn/alpine";
const US_ALPINE_MIRROR: &str = "https://dl-cdn.alpinelinux.org/alpine";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApkRegion {
    China,
    Us,
}

impl ApkRegion {
    pub(crate) fn canonical_name(self) -> &'static str {
        match self {
            Self::China => "china",
            Self::Us => "us",
        }
    }

    fn mirror_base(self) -> &'static str {
        match self {
            Self::China => CHINA_ALPINE_MIRROR,
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
        None => Ok(ApkRegion::China),
        Some(value) if matches!(value.as_str(), "china" | "cn") => Ok(ApkRegion::China),
        Some(value) if matches!(value.as_str(), "us" | "usa") => Ok(ApkRegion::Us),
        Some(value) => bail!(
            "unsupported {STARRY_APK_REGION_VAR} `{value}`; supported values are: china, cn, us, \
             usa"
        ),
    }
}

pub(crate) fn rewrite_apk_repositories_for_region(
    staging_root: &Path,
    region: ApkRegion,
) -> anyhow::Result<()> {
    let path = staging_root.join("etc/apk/repositories");
    let original =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let rewritten = rewrite_apk_repositories_content(&original, region);

    fs::write(&path, rewritten).with_context(|| format!("failed to write {}", path.display()))
}

pub(crate) fn rewrite_apk_repositories_content(original: &str, region: ApkRegion) -> String {
    let ends_with_newline = original.ends_with('\n');
    let rewritten = original
        .lines()
        .map(|line| rewrite_apk_repository_line(line, region))
        .collect::<Vec<_>>()
        .join("\n");

    if ends_with_newline {
        rewritten + "\n"
    } else {
        rewritten
    }
}

fn rewrite_apk_repository_line(line: &str, region: ApkRegion) -> String {
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

    format!(
        "{}{}/{}{}",
        &line[..leading_len],
        region.mirror_base(),
        suffix,
        trailing
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn parse_apk_region_defaults_to_china() {
        assert_eq!(parse_apk_region(None).unwrap(), ApkRegion::China);
        assert_eq!(parse_apk_region(Some("")).unwrap(), ApkRegion::China);
    }

    #[test]
    fn parse_apk_region_accepts_supported_aliases() {
        assert_eq!(parse_apk_region(Some("china")).unwrap(), ApkRegion::China);
        assert_eq!(parse_apk_region(Some("cn")).unwrap(), ApkRegion::China);
        assert_eq!(parse_apk_region(Some("us")).unwrap(), ApkRegion::Us);
        assert_eq!(parse_apk_region(Some("usa")).unwrap(), ApkRegion::Us);
    }

    #[test]
    fn parse_apk_region_rejects_unknown_value() {
        let err = parse_apk_region(Some("europe")).unwrap_err().to_string();
        assert!(err.contains(STARRY_APK_REGION_VAR));
        assert!(err.contains("china, cn, us, usa"));
    }

    #[test]
    fn rewrite_apk_repositories_switches_to_us_mirror() {
        let input = "https://mirrors.cernet.edu.cn/alpine/v3.23/main\nhttps://mirrors.cernet.edu.cn/alpine/v3.23/community\n";

        assert_eq!(
            rewrite_apk_repositories_content(input, ApkRegion::Us),
            "https://dl-cdn.alpinelinux.org/alpine/v3.23/main\nhttps://dl-cdn.alpinelinux.org/alpine/v3.23/community\n"
        );
    }

    #[test]
    fn rewrite_apk_repositories_switches_to_china_mirror() {
        let input = "https://dl-cdn.alpinelinux.org/alpine/v3.23/main\nhttps://dl-cdn.alpinelinux.org/alpine/v3.23/community\n";

        assert_eq!(
            rewrite_apk_repositories_content(input, ApkRegion::China),
            "https://mirrors.cernet.edu.cn/alpine/v3.23/main\nhttps://mirrors.cernet.edu.cn/alpine/v3.23/community\n"
        );
    }

    #[test]
    fn rewrite_apk_repositories_preserves_comments_blank_lines_and_other_urls() {
        let input = "\n# keep me\n  https://example.com/not-alpine/main  \nhttps://mirror.nyist.edu.cn/alpine/v3.23/main\n";

        assert_eq!(
            rewrite_apk_repositories_content(input, ApkRegion::Us),
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

        rewrite_apk_repositories_for_region(root.path(), ApkRegion::Us).unwrap();

        assert_eq!(
            fs::read_to_string(&repositories).unwrap(),
            "https://dl-cdn.alpinelinux.org/alpine/v3.23/main\nhttps://dl-cdn.alpinelinux.org/alpine/v3.23/community\n"
        );
    }
}
