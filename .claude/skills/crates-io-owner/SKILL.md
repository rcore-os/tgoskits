---
name: crates-io-owner
description: Audit or update crates.io owners for newly added crates in this tgoskits workspace. Use this skill when the user wants to add or verify `github:rcore-os:crates-io` on branch-added crates, asks which new crates still need the team owner, or explicitly says to use `cargo owner` instead of editing `Cargo.toml`.
---

# Crates.io Owner

This skill handles crates.io owner management for newly added crates by using `cargo owner` directly. Do not encode the owner in `Cargo.toml` for this workflow.

## Workflow

1. Identify branch-added `Cargo.toml` files relative to the comparison base, usually:
   ```bash
   git diff --name-status origin/main...HEAD
   ```
2. Narrow that list to real crate manifests that are relevant to publishing:
   - Prefer workspace member crates.
   - Skip standalone examples, test fixtures, and helper crates unless the user explicitly includes them.
   - Skip crates with `publish = false`.
3. Determine the crate names from `cargo metadata` or the manifests themselves.
4. Add the owner with `cargo owner`, one crate at a time:
   ```bash
   cargo owner --add github:rcore-os:crates-io <crate>
   ```
5. Treat these outcomes carefully:
   - `already an owner`: success/no-op; report that the owner was already present.
   - other registry errors: surface them clearly and stop or continue based on severity.
6. Do not modify `Cargo.toml` just to record crates.io ownership for this task.

## Recommended Commands

List added manifests:

```bash
python3 - <<'PY'
import subprocess
out = subprocess.check_output(
    ['git', 'diff', '--name-status', 'origin/main...HEAD'],
    text=True,
)
for line in out.splitlines():
    status, path = line.split('\t', 1)
    if status == 'A' and path.endswith('Cargo.toml'):
        print(path)
PY
```

Resolve candidate workspace packages:

```bash
cargo metadata --no-deps --format-version 1
```

Add the owner:

```bash
cargo owner --add github:rcore-os:crates-io <crate>
```

## Reporting

Summarize results in three buckets when helpful:

- owner added successfully
- already owned by `github:rcore-os:crates-io`
- skipped or blocked, with the reason

If no file changes were needed, state that explicitly.

## Guardrails

- Use the crates.io operation the user asked for; do not replace it with manifest metadata edits.
- Keep the scope tight to branch-added crates unless the user asks for a broader audit.
- If the comparison base is unclear, prefer `origin/main...HEAD` unless the branch context suggests another base.
- If a crate is unpublished and `cargo owner --add` fails because the crate does not exist on crates.io yet, report that plainly instead of inventing a local workaround.
