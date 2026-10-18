#!/usr/bin/env bash
# Подготовить релиз: scripts/release.sh 0.2.0 [--push]
#
# 1. Версия workspace → X.Y.Z (Cargo.toml + Cargo.lock).
# 2. CHANGELOG: раздел [Unreleased] становится [X.Y.Z] - <дата>.
# 3. Коммит "chore: release vX.Y.Z" и аннотированный тег vX.Y.Z.
# С --push тег и main уходят на origin → release.yml собирает черновик релиза.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION="${1:?usage: release.sh <X.Y.Z> [--push]}"
PUSH="${2:-}"
TODAY="$(date +%Y-%m-%d)"

die() { echo "error: $*" >&2; exit 1; }

[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "version must be X.Y.Z, got '$VERSION'"
[[ -z "$PUSH" || "$PUSH" == "--push" ]] || die "unknown argument: $PUSH"
cd "$ROOT"
[[ "$(git branch --show-current)" == "main" ]] || die "release from main only"
[[ -z "$(git status --porcelain)" ]] || die "working tree is not clean"
git rev-parse -q --verify "refs/tags/v$VERSION" >/dev/null && die "tag v$VERSION already exists"

# Раздел Unreleased не должен быть пустым: иначе релиз без заметок.
unreleased="$(awk '/^## \[Unreleased\]/ { f = 1; next } /^## \[/ { f = 0 } f' CHANGELOG.md)"
[[ -n "${unreleased//[[:space:]]/}" ]] || die "CHANGELOG.md: [Unreleased] is empty"

# Версия — единственная строка `version = ` в [workspace.package].
sed -i '' -E "/^\[workspace\.package\]/,/^\[/ s/^version = \".*\"/version = \"$VERSION\"/" Cargo.toml
cargo update --workspace --quiet
sed -i '' "s/^## \[Unreleased\]$/## [Unreleased]\n\n## [$VERSION] - $TODAY/" CHANGELOG.md

cargo check --workspace --locked --quiet
git add Cargo.toml Cargo.lock CHANGELOG.md
git commit -m "chore: release v$VERSION"
git tag -a "v$VERSION" -m "crabtrap $VERSION"

if [[ "$PUSH" == "--push" ]]; then
    git push origin main "v$VERSION"
else
    echo "Ready. Publish with: git push origin main v$VERSION"
fi
