//! Frozen inputs and run modes for the Task-3 manual-versus-YOLO experiment.

const MANIFEST: &str = include_str!("../../../../../scripts/task3/task3-ab-manifest.tsv");
const INSTALLED_INPUT_DIR: &str = "/usr/share/task3-yolo/task3-ab";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RunMode {
    Normal,
    ModelOnly,
    Manual,
    Yolo,
    OutOfOrder,
    InvalidParameter,
    ModelRejected,
}

impl RunMode {
    pub(crate) fn parse(value: Option<&str>) -> Result<Self, &'static str> {
        match value {
            None | Some("normal") => Ok(Self::Normal),
            Some("model-only") => Ok(Self::ModelOnly),
            Some("manual") => Ok(Self::Manual),
            Some("yolo") => Ok(Self::Yolo),
            Some("out-of-order") => Ok(Self::OutOfOrder),
            Some("invalid-parameter") => Ok(Self::InvalidParameter),
            Some("model-rejected") => Ok(Self::ModelRejected),
            Some(_) => Err(
                "mode must be normal, model-only, manual, yolo, out-of-order, invalid-parameter, \
                 or model-rejected",
            ),
        }
    }

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::ModelOnly => "model-only",
            Self::Manual => "manual",
            Self::Yolo => "yolo",
            Self::OutOfOrder => "out-of-order",
            Self::InvalidParameter => "invalid-parameter",
            Self::ModelRejected => "model-rejected",
        }
    }

    pub(crate) const fn is_ab_experiment(self) -> bool {
        matches!(self, Self::Manual | Self::Yolo)
    }

    pub(crate) const fn requires_model(self) -> bool {
        !matches!(self, Self::Manual)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExpectedBehavior {
    Accept,
    Reject,
}

impl ExpectedBehavior {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Reject => "reject",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TargetSource {
    Manual,
    Yolo,
}

impl TargetSource {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Yolo => "yolo",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ImageSample {
    pub(crate) id: String,
    pub(crate) filename: String,
    pub(crate) sha256: String,
    pub(crate) truth_target: Option<i32>,
    pub(crate) expected: ExpectedBehavior,
}

impl ImageSample {
    pub(crate) fn installed_path(&self) -> String {
        format!("{INSTALLED_INPUT_DIR}/{}", self.filename)
    }
}

pub(crate) fn load_task3_ab_manifest() -> Result<Vec<ImageSample>, String> {
    parse_manifest(MANIFEST)
}

fn parse_manifest(contents: &str) -> Result<Vec<ImageSample>, String> {
    let mut samples = Vec::new();
    for (line_index, line) in contents.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 5 {
            return Err(format!(
                "Task-3 A/B manifest line {} must have five tab-separated fields",
                line_index + 1
            ));
        }
        let [id, filename, sha256, truth_target, expected] =
            <[&str; 5]>::try_from(fields.as_slice()).map_err(|_| {
                format!(
                    "Task-3 A/B manifest line {} has invalid fields",
                    line_index + 1
                )
            })?;
        if id.is_empty() || filename.is_empty() || filename.contains('/') {
            return Err(format!(
                "Task-3 A/B manifest line {} has an invalid id or filename",
                line_index + 1
            ));
        }
        if sha256.len() != 64
            || !sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(format!(
                "Task-3 A/B manifest line {} has an invalid SHA256",
                line_index + 1
            ));
        }
        let truth_target = match truth_target {
            "none" => None,
            value => Some(value.parse::<i32>().map_err(|_| {
                format!(
                    "Task-3 A/B manifest line {} has an invalid truth target",
                    line_index + 1
                )
            })?),
        };
        if truth_target.is_some_and(|target| !(0..=1000).contains(&target)) {
            return Err(format!(
                "Task-3 A/B manifest line {} has an out-of-range truth target",
                line_index + 1
            ));
        }
        let expected = match expected {
            "accept" => ExpectedBehavior::Accept,
            "reject" => ExpectedBehavior::Reject,
            _ => {
                return Err(format!(
                    "Task-3 A/B manifest line {} has an invalid expected behavior",
                    line_index + 1
                ));
            }
        };
        samples.push(ImageSample {
            id: id.to_owned(),
            filename: filename.to_owned(),
            sha256: sha256.to_owned(),
            truth_target,
            expected,
        });
    }
    if samples.is_empty() {
        return Err("Task-3 A/B manifest has no samples".to_owned());
    }
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_manifest_with_non_hexadecimal_hash() {
        let invalid = concat!(
            "left\tleft.ppm\t",
            "gggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggg",
            "\t200\taccept\n"
        );

        assert!(parse_manifest(invalid).is_err());
    }

    #[test]
    fn rejects_manifest_with_path_traversal() {
        let invalid = concat!(
            "left\t../left.ppm\t",
            "061000254f73981a2df5dd902cb635a1e8efb7977f66820e109b8551e7f9a988",
            "\t200\taccept\n"
        );

        assert!(parse_manifest(invalid).is_err());
    }
}
