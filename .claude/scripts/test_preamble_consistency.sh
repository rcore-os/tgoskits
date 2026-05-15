#!/usr/bin/env bash
# test_preamble_consistency.sh
# Verify that all 6 agent files have a standardized preamble block with consistent structure.
# Exits 0 on success, 1 on first failure with a descriptive error message.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AGENTS_DIR="$(cd "$SCRIPT_DIR/../agents" && pwd)"

AGENTS=(
  "pr-review.md"
  "bug-hunt.md"
  "impl.md"
  "driver-audit.md"
  "test-gen.md"
  "self-evolve.md"
)

err() {
  echo "FAIL: $1" >&2
  exit 1
}

get_body() {
  sed -n '/^---$/,/^---$/!p' "$1"
}

get_preamble() {
  get_body "$1" | head -40
}

# Check 1: Each agent file has '### Dependency Check' after frontmatter
for agent in "${AGENTS[@]}"; do
  file="$AGENTS_DIR/$agent"
  [ -f "$file" ] || err "Agent file not found: $file"

  first_line=""
  while IFS= read -r line; do
    if [ -n "$line" ]; then
      first_line="$line"
      break
    fi
  done < <(get_body "$file")

  if ! echo "$first_line" | grep -q "^### Dependency Check"; then
    err "$agent: first non-empty line after frontmatter is not '### Dependency Check' (got: '$first_line')"
  fi
done

# Check 2: No fenced code blocks in preamble section
# The preamble starts with '### Dependency Check' and ends at the next '#' heading
for agent in "${AGENTS[@]}"; do
  file="$AGENTS_DIR/$agent"
  body=$(get_body "$file")

  # Extract lines from '### Dependency Check' to the next '#' heading (exclusive)
  preamble_section=$(printf '%s\n' "$body" | sed -n '/^### Dependency Check/,/^#/{/^### Dependency Check/p;/^#/!p}' | head -n -1)

  code_block_count=$(echo "$preamble_section" | grep -c '^```' || true)
  if [ "$code_block_count" -gt 0 ]; then
    err "$agent: preamble section contains $code_block_count fenced code blocks (expected 0)"
  fi
done

# Check 3: Preamble region contains all three section headers
for agent in "${AGENTS[@]}"; do
  file="$AGENTS_DIR/$agent"
  preamble=$(get_preamble "$file")

  if ! echo "$preamble" | grep -q "\*\*Skills\*\*"; then
    err "$agent: preamble missing '**Skills**' section header"
  fi
  if ! echo "$preamble" | grep -q "\*\*Tools\*\*"; then
    err "$agent: preamble missing '**Tools**' section header"
  fi
  if ! echo "$preamble" | grep -q "\*\*Agents\*\*"; then
    err "$agent: preamble missing '**Agents**' section header"
  fi
done

# Check 4: Preamble region contains 'AGENT ABORTED:'
for agent in "${AGENTS[@]}"; do
  file="$AGENTS_DIR/$agent"
  preamble=$(get_preamble "$file")

  if ! echo "$preamble" | grep -q "AGENT ABORTED:"; then
    err "$agent: preamble missing 'AGENT ABORTED:' pattern"
  fi
done

# Check 5: Zero occurrences of 'security-auditor' across all agent files
sa_count=$(grep -rn "security-auditor" "$AGENTS_DIR" 2>/dev/null | wc -l)
if [ "$sa_count" -ne 0 ]; then
  err "Found $sa_count occurrence(s) of 'security-auditor' in agent files (expected 0)"
fi

echo "PASS: preamble consistency verified"
exit 0
