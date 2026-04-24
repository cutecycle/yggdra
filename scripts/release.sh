#!/usr/bin/env bash
# release.sh — test, commit, tag, and push to trigger the release workflow.
# Usage: ./scripts/release.sh [commit message]
# The version is read from Cargo.toml automatically.

set -euo pipefail

VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
TAG="v${VERSION}"
MSG="${1:-release ${TAG}}"

echo "→ version  : ${TAG}"
echo "→ message  : ${MSG}"
echo ""

# Ensure we're on main and the tree is not in a broken state
BRANCH=$(git rev-parse --abbrev-ref HEAD)
if [[ "$BRANCH" != "main" ]]; then
    echo "✗ not on main (currently on ${BRANCH})" >&2
    exit 1
fi

# Run tests before touching git
echo "→ running tests…"
cargo test --lib --quiet
echo "✓ tests pass"

# Refuse to push if the tag already exists (local or remote)
if git rev-parse "${TAG}" >/dev/null 2>&1; then
    echo "✗ tag ${TAG} already exists locally — bump Cargo.toml version first" >&2
    exit 1
fi

# Stage everything, commit if there are changes
if ! git diff --quiet || ! git diff --cached --quiet || git ls-files --others --exclude-standard | grep -q .; then
    git add -A
    git commit -m "${MSG}

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
    echo "✓ committed"
else
    echo "→ nothing to commit, tagging HEAD"
fi

git tag "${TAG}"
echo "✓ tagged ${TAG}"

git push origin main --tags
echo "✓ pushed — release workflow will build cross-platform binaries"
