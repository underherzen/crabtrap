#!/usr/bin/env bash
# Раздел CHANGELOG.md для версии (без заголовка): scripts/changelog-section.sh 0.1.0
set -euo pipefail

VERSION="${1:?usage: changelog-section.sh <version>}"
CHANGELOG="$(dirname "$0")/../CHANGELOG.md"

section="$(awk -v ver="$VERSION" '
    /^## \[/ { if (found) exit; found = index($0, "## [" ver "]") == 1; next }
    found { print }
' "$CHANGELOG")"

if [[ -z "${section//[[:space:]]/}" ]]; then
    echo "CHANGELOG.md has no entries for $VERSION" >&2
    exit 1
fi
printf '%s\n' "$section"
