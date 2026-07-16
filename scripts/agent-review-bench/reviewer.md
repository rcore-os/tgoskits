# Offline Review Contract

This file marks the repository as the isolated `review-single-pr offline-benchmark` environment.

- The review target is the committed change between `bench-base` and `HEAD` in this repository.
- There is no live PR identity or external context. Network access, GitHub operations, paths outside
  this repository, writes, builds, and tests are unavailable.
- The current `review-single-pr` skill, `AGENTS.md`, `book/guideline/`, and
  `.agent-review-context/review.schema.json` are committed on both sides of the synthetic change and
  therefore are review instructions rather than part of the target diff.
- The final response must be only the JSON object required by the supplied output schema.
