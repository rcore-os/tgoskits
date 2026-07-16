use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

use super::cases::BenchCase;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReviewOutput {
    pub(super) summary: String,
    pub(super) findings: Vec<ReviewFinding>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReviewFinding {
    pub(super) title: String,
    pub(super) body: String,
    pub(super) path: String,
    pub(super) line: usize,
    pub(super) severity: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GradeOutput {
    pub(super) matches: Vec<FindingMatch>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FindingMatch {
    pub(super) expected_id: String,
    pub(super) finding_indices: Vec<usize>,
    pub(super) reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CaseScore {
    pub(super) caught: usize,
    pub(super) expected: usize,
    pub(super) extra_findings: usize,
}

pub(super) fn score_review(
    case: &BenchCase,
    review: &ReviewOutput,
    grade: &GradeOutput,
) -> anyhow::Result<CaseScore> {
    let expected_ids = case
        .expected
        .iter()
        .map(|expected| expected.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut matches = BTreeMap::new();
    for finding_match in &grade.matches {
        if !expected_ids.contains(finding_match.expected_id.as_str()) {
            bail!(
                "grader returned unknown expected finding `{}`",
                finding_match.expected_id
            );
        }
        if matches
            .insert(finding_match.expected_id.as_str(), finding_match)
            .is_some()
        {
            bail!(
                "grader returned duplicate match for `{}`",
                finding_match.expected_id
            );
        }
        if finding_match.reason.trim().is_empty() {
            bail!(
                "grader returned an empty reason for `{}`",
                finding_match.expected_id
            );
        }
        let mut unique_indices = BTreeSet::new();
        for &index in &finding_match.finding_indices {
            if !unique_indices.insert(index) {
                bail!(
                    "grader returned duplicate review finding index {index} for `{}`",
                    finding_match.expected_id
                );
            }
            review.findings.get(index).with_context(|| {
                format!(
                    "grader referenced review finding index {index}, but only {} findings exist",
                    review.findings.len()
                )
            })?;
        }
    }
    if matches.len() != expected_ids.len() {
        let missing = expected_ids
            .into_iter()
            .filter(|id| !matches.contains_key(id))
            .collect::<Vec<_>>();
        bail!("grader omitted expected finding(s): {}", missing.join(", "));
    }

    let matched_indices = matches
        .values()
        .flat_map(|finding_match| finding_match.finding_indices.iter().copied())
        .collect::<BTreeSet<_>>();
    Ok(CaseScore {
        caught: matches
            .values()
            .filter(|finding_match| !finding_match.finding_indices.is_empty())
            .count(),
        expected: case.expected.len(),
        extra_findings: review.findings.len() - matched_indices.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_review_bench::cases::{ExpectedFinding, Severity};

    #[test]
    fn scores_joint_matches_shared_findings_and_extras() {
        let case = sample_case();
        let review = ReviewOutput {
            summary: "summary".into(),
            findings: vec![
                finding("first part"),
                finding("shared part"),
                finding("extra"),
            ],
        };
        let grade = GradeOutput {
            matches: vec![
                finding_match("first", &[0, 1]),
                finding_match("second", &[1]),
            ],
        };

        assert_eq!(
            score_review(&case, &review, &grade).unwrap(),
            CaseScore {
                caught: 2,
                expected: 2,
                extra_findings: 1,
            }
        );
    }

    #[test]
    fn scores_all_missed_and_zero_candidate_reviews() {
        let case = sample_case();
        let missed_review = ReviewOutput {
            summary: "summary".into(),
            findings: vec![finding("unmatched")],
        };
        let missed_grade = GradeOutput {
            matches: vec![finding_match("first", &[]), finding_match("second", &[])],
        };
        assert_eq!(
            score_review(&case, &missed_review, &missed_grade).unwrap(),
            CaseScore {
                caught: 0,
                expected: 2,
                extra_findings: 1,
            }
        );

        let empty_review = ReviewOutput {
            summary: "summary".into(),
            findings: Vec::new(),
        };
        assert_eq!(
            score_review(&case, &empty_review, &missed_grade).unwrap(),
            CaseScore {
                caught: 0,
                expected: 2,
                extra_findings: 0,
            }
        );
    }

    #[test]
    fn rejects_unknown_missing_duplicate_and_invalid_matches() {
        let case = sample_case();
        let review = ReviewOutput {
            summary: "summary".into(),
            findings: vec![finding("caught")],
        };
        let unknown = GradeOutput {
            matches: vec![finding_match("unknown", &[0])],
        };
        assert!(score_review(&case, &review, &unknown).is_err());

        let missing = GradeOutput {
            matches: vec![finding_match("first", &[0])],
        };
        assert!(score_review(&case, &review, &missing).is_err());

        let duplicate_expected = GradeOutput {
            matches: vec![
                finding_match("first", &[0]),
                finding_match("first", &[0]),
                finding_match("second", &[]),
            ],
        };
        assert!(score_review(&case, &review, &duplicate_expected).is_err());

        let out_of_range = GradeOutput {
            matches: vec![finding_match("first", &[2]), finding_match("second", &[])],
        };
        assert!(score_review(&case, &review, &out_of_range).is_err());

        let duplicate_index = GradeOutput {
            matches: vec![
                finding_match("first", &[0, 0]),
                finding_match("second", &[]),
            ],
        };
        assert!(score_review(&case, &review, &duplicate_index).is_err());

        let empty_reason = GradeOutput {
            matches: vec![
                FindingMatch {
                    expected_id: "first".into(),
                    finding_indices: vec![0],
                    reason: " ".into(),
                },
                finding_match("second", &[]),
            ],
        };
        assert!(score_review(&case, &review, &empty_reason).is_err());
    }

    fn sample_case() -> BenchCase {
        BenchCase {
            id: "0001-sample".into(),
            pr: 1,
            title: "sample".into(),
            remote: "https://example.invalid/repo.git".into(),
            base: "a".repeat(40),
            head: "b".repeat(40),
            source: "source".into(),
            fixed_by: "c".repeat(40),
            expected: vec![expected("first"), expected("second")],
        }
    }

    fn expected(id: &str) -> ExpectedFinding {
        ExpectedFinding {
            id: id.into(),
            path: "src/lib.rs".into(),
            line: 1,
            severity: Severity::Major,
            description: "description".into(),
        }
    }

    fn finding(title: &str) -> ReviewFinding {
        ReviewFinding {
            title: title.into(),
            body: "body".into(),
            path: "src/lib.rs".into(),
            line: 1,
            severity: "major".into(),
        }
    }

    fn finding_match(expected_id: &str, finding_indices: &[usize]) -> FindingMatch {
        FindingMatch {
            expected_id: expected_id.into(),
            finding_indices: finding_indices.to_vec(),
            reason: "reason".into(),
        }
    }
}
