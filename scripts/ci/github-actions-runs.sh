#!/usr/bin/env bash

set -euo pipefail

DEFAULT_CI_WORKFLOW_PATH=".github/workflows/ci.yml"

usage() {
    cat <<'EOF'
Usage:
  scripts/ci/github-actions-runs.sh should-skip-push-ci [branch]
  scripts/ci/github-actions-runs.sh cancel-duplicate-push-for-pr

Environment:
  GITHUB_REPOSITORY        owner/name repository slug.
  GITHUB_REF_NAME          Push branch name for should-skip-push-ci.
  PR_HEAD_REF             Pull request head branch for cancel-duplicate-push-for-pr.
  PR_HEAD_SHA             Pull request head SHA for cancel-duplicate-push-for-pr.
  PR_HEAD_REPOSITORY      Pull request head repository slug for same-repo guard.
  CI_WORKFLOW_PATH        Workflow path or file name to inspect. Defaults to .github/workflows/ci.yml.
  GH_CI_OPEN_PRS_JSON     Optional JSON array used by should-skip-push-ci tests.
  GH_CI_RUNS_JSON         Optional JSON array used by cancel-duplicate-push-for-pr tests.
  GH_CI_DRY_RUN           If true, print cancel targets without calling the API.
EOF
}

require_env() {
    local name="$1"
    if [ -z "${!name:-}" ]; then
        echo "Missing required environment variable: ${name}" >&2
        exit 2
    fi
}

repo_owner() {
    printf '%s' "${GITHUB_REPOSITORY%%/*}"
}

repo_name() {
    printf '%s' "${GITHUB_REPOSITORY#*/}"
}

ci_workflow_id() {
    local workflow_path
    workflow_path="${CI_WORKFLOW_PATH:-$DEFAULT_CI_WORKFLOW_PATH}"
    printf '%s' "${workflow_path##*/}"
}

append_summary() {
    if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
        printf '%s\n' "$@" >> "$GITHUB_STEP_SUMMARY"
    fi
}

load_open_prs_json() {
    local branch="$1"

    if [ -n "${GH_CI_OPEN_PRS_JSON:-}" ]; then
        printf '%s\n' "$GH_CI_OPEN_PRS_JSON"
        return
    fi

    gh pr list \
        --repo "$GITHUB_REPOSITORY" \
        --head "$branch" \
        --state open \
        --json number,headRefName,headRepository,headRepositoryOwner,isCrossRepository
}

same_repo_pr_count() {
    local branch="$1"
    local owner name
    owner="$(repo_owner)"
    name="$(repo_name)"

    load_open_prs_json "$branch" |
        BRANCH="$branch" REPO_OWNER="$owner" REPO_NAME="$name" jq '
            [
              .[]
              | select(.headRefName == env.BRANCH)
              | select((.headRepositoryOwner.login // "" | ascii_downcase) == (env.REPO_OWNER | ascii_downcase))
              | select((.headRepository.name // "" | ascii_downcase) == (env.REPO_NAME | ascii_downcase))
            ]
            | length
        '
}

should_skip_push_ci() {
    require_env GITHUB_REPOSITORY

    local branch="${1:-${GITHUB_REF_NAME:-}}"
    if [ -z "$branch" ]; then
        echo "Missing branch: pass an argument or set GITHUB_REF_NAME" >&2
        exit 2
    fi

    local count
    count="$(same_repo_pr_count "$branch")"
    if [ "$count" != "0" ]; then
        append_summary \
            "## Duplicate push CI skipped" \
            "" \
            "Branch \`${branch}\` already has an open same-repository pull request. The pull_request CI run will validate this commit."
        return 0
    fi

    return 1
}

load_workflow_runs_json() {
    local workflow_id="$1"
    local branch="$2"
    local status="$3"

    if [ -n "${GH_CI_RUNS_JSON:-}" ]; then
        printf '%s\n' "$GH_CI_RUNS_JSON"
        return
    fi

    gh api --method GET \
        "repos/${GITHUB_REPOSITORY}/actions/workflows/${workflow_id}/runs" \
        -f event=push \
        -f branch="$branch" \
        -f status="$status" \
        -f per_page=100 \
        --jq '.workflow_runs'
}

matching_push_run_ids() {
    local workflow_id="$1"
    local branch="$2"
    local sha="$3"
    local status="$4"

    load_workflow_runs_json "$workflow_id" "$branch" "$status" |
        HEAD_SHA="$sha" jq -r '
            .[]
            | select(.event == "push")
            | select(.head_sha == env.HEAD_SHA)
            | select(.status != "completed")
            | .id
        '
}

cancel_run() {
    local run_id="$1"

    if [ "${GH_CI_DRY_RUN:-false}" = "true" ]; then
        echo "Would cancel workflow run ${run_id}"
        return
    fi

    if gh api --method POST "repos/${GITHUB_REPOSITORY}/actions/runs/${run_id}/cancel" >/dev/null; then
        echo "Canceled workflow run ${run_id}"
    else
        echo "Warning: failed to cancel workflow run ${run_id}" >&2
    fi
}

cancel_duplicate_push_for_pr() {
    require_env GITHUB_REPOSITORY
    require_env PR_HEAD_REF
    require_env PR_HEAD_SHA
    require_env PR_HEAD_REPOSITORY

    if [ "$PR_HEAD_REPOSITORY" != "$GITHUB_REPOSITORY" ]; then
        append_summary \
            "## Duplicate push CI cancellation skipped" \
            "" \
            "The pull request comes from \`${PR_HEAD_REPOSITORY}\`, so no same-repository push run can duplicate it."
        echo "Skipping duplicate push cancellation for cross-repository PR head ${PR_HEAD_REPOSITORY}."
        return
    fi

    local workflow_id
    workflow_id="$(ci_workflow_id)"

    local statuses run_ids run_id canceled_count
    statuses=(queued in_progress pending waiting requested)
    run_ids="$(
        for status in "${statuses[@]}"; do
            matching_push_run_ids "$workflow_id" "$PR_HEAD_REF" "$PR_HEAD_SHA" "$status"
        done | sort -u
    )"

    canceled_count=0
    if [ -n "$run_ids" ]; then
        while IFS= read -r run_id; do
            [ -n "$run_id" ] || continue
            cancel_run "$run_id"
            canceled_count=$((canceled_count + 1))
        done <<< "$run_ids"
    fi

    if [ "$canceled_count" -eq 0 ]; then
        echo "No unfinished push CI run matched branch ${PR_HEAD_REF} at ${PR_HEAD_SHA}."
        append_summary \
            "## Duplicate push CI cancellation" \
            "" \
            "No unfinished push CI run matched branch \`${PR_HEAD_REF}\` at \`${PR_HEAD_SHA}\`."
    else
        append_summary \
            "## Duplicate push CI cancellation" \
            "" \
            "Canceled ${canceled_count} unfinished push CI run(s) for branch \`${PR_HEAD_REF}\` at \`${PR_HEAD_SHA}\`."
    fi
}

main() {
    local command="${1:-}"
    case "$command" in
        should-skip-push-ci)
            shift
            should_skip_push_ci "$@"
            ;;
        cancel-duplicate-push-for-pr)
            shift
            if [ "$#" -ne 0 ]; then
                usage >&2
                exit 2
            fi
            cancel_duplicate_push_for_pr
            ;;
        -h|--help|help)
            usage
            ;;
        *)
            usage >&2
            exit 2
            ;;
    esac
}

main "$@"
