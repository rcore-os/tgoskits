# Offline Review Contract

Review only the committed change between `bench-base` and `HEAD` in this repository.

- This is an offline benchmark snapshot, not a live GitHub pull request. Do not use the network,
  `gh`, GitHub APIs, external repositories, or paths outside this repository. Do not submit a
  review, comment, issue, branch, commit, or reviewer request.
- Do not modify files. Use read-only source inspection and Git history/diff commands only; do not
  build or run tests that write artifacts.
- Read and apply the repository `AGENTS.md` and every file under `book/guideline/` as the current
  review standards. Do not invoke the side-effectful `review-single-pr` workflow.
- Review the change for actionable correctness, concurrency, security/soundness, hardware or ABI
  semantics, maintainability, deterministic regression coverage, test discovery/wiring, and
  documentation/user-facing compatibility.
- Report only issues introduced by this change. Each finding must explain the concrete failure,
  its impact, and the expected fix direction, and must point to a changed line on the `HEAD` side.
- Return the JSON object required by the supplied output schema. Use an empty `findings` array when
  no actionable issue exists.
