use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
#[serde(rename_all = "kebab-case")]
pub(super) struct AxplatMetadata {
    pub(super) platform: String,
    pub(super) arch: String,
    pub(super) default_for_arch: bool,
    pub(super) dynamic: bool,
}

pub(super) fn platform_feature_names(metadata: &cargo_metadata::Metadata) -> Vec<String> {
    let mut platforms = platform_metadata_entries(metadata)
        .into_iter()
        .map(|platform| platform.platform)
        .collect::<Vec<_>>();
    platforms.sort();
    platforms.dedup();
    platforms
}

pub(super) fn platform_metadata_entries(
    metadata: &cargo_metadata::Metadata,
) -> Vec<AxplatMetadata> {
    metadata
        .packages
        .iter()
        .filter_map(|package| {
            package
                .metadata
                .get("axplat")
                .cloned()
                .and_then(|metadata| serde_json::from_value::<AxplatMetadata>(metadata).ok())
        })
        .collect()
}
