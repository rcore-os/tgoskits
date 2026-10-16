#!/bin/bash
set -euo pipefail

app_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
validator="$app_dir/validate_sha256.sh"
valid_sha=892439b839e2cfb6cdc71282b1d9e5dc94730e6f4b6db4cce6597b3972e6bde3
upper_sha="$(printf '%s' "$valid_sha" | tr '[:lower:]' '[:upper:]')"

"$validator" "$valid_sha"

for invalid_sha in "${valid_sha}0" "${valid_sha%?}" "$upper_sha" "${valid_sha%?}g"; do
    if "$validator" "$invalid_sha"; then
        echo "accepted invalid SHA-256: $invalid_sha" >&2
        exit 1
    fi
done

echo "validate_sha256_lower_hex=PASS"
