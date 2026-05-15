#!/usr/bin/env bash
set -euo pipefail

# test_frontmatter_tools.sh
# Verify that pr-review and bug-hunt agents have WebSearch, WebFetch tools
# and that pr-review has superpowers:systematic-debugging skill in frontmatter.
# Exits 0 on success, 1 on first failure with a descriptive error message.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AGENTS_DIR="$(cd "$SCRIPT_DIR/../agents" && pwd)"

err() {
  echo "FAIL: $1" >&2
  exit 1
}

# Helper: extract a YAML field from frontmatter
# Extracts all lines between 'field:' and the next field starting at column 0 (or end of frontmatter)
extract_yaml_field() {
  local file="$1"
  local field="$2"
  # Extract from "field:" to the closing "---" of frontmatter,
  # then remove the closing "---" line itself.
  sed -n "/^${field}:/,/^---$/p" "$file" | sed '$d'
}

# 1. pr-review.md: WebSearch in tools
pr_file="$AGENTS_DIR/pr-review.md"
[ -f "$pr_file" ] || err "pr-review.md not found"

pr_tools=$(extract_yaml_field "$pr_file" "tools")
if ! echo "$pr_tools" | grep -q "WebSearch"; then
  err "pr-review.md: tools list missing WebSearch"
fi

# 2. pr-review.md: WebFetch in tools
if ! echo "$pr_tools" | grep -q "WebFetch"; then
  err "pr-review.md: tools list missing WebFetch"
fi

# 3. bug-hunt.md: WebSearch in tools
bh_file="$AGENTS_DIR/bug-hunt.md"
[ -f "$bh_file" ] || err "bug-hunt.md not found"

bh_tools=$(extract_yaml_field "$bh_file" "tools")
if ! echo "$bh_tools" | grep -q "WebSearch"; then
  err "bug-hunt.md: tools list missing WebSearch"
fi

# 4. bug-hunt.md: WebFetch in tools
if ! echo "$bh_tools" | grep -q "WebFetch"; then
  err "bug-hunt.md: tools list missing WebFetch"
fi

# 5. pr-review.md: superpowers:systematic-debugging in skills
pr_skills=$(extract_yaml_field "$pr_file" "skills")
if ! echo "$pr_skills" | grep -q "superpowers:systematic-debugging"; then
  err "pr-review.md: skills list missing superpowers:systematic-debugging"
fi

echo "PASS: frontmatter tools/skills verified"
exit 0
