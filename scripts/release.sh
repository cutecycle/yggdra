#!/usr/bin/env bash
# release.sh — test, commit, tag, and push to trigger the release workflow.
# Usage: ./scripts/release.sh [--dry-run] [commit message]
#   --dry-run  Run all tests and build checks but skip git commit/tag/push.
# The version is read from Cargo.toml automatically.
# API-dependent tests use OLLAMA_ENDPOINT (default: http://localhost:11434).

set -euo pipefail

DRY_RUN=false
if [[ "${1:-}" == "--dry-run" ]]; then
    DRY_RUN=true
    shift
fi

VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
TAG="v${VERSION}"
MSG="${1:-release ${TAG}}"

echo "→ version  : ${TAG}"
echo "→ message  : ${MSG}"
[[ "$DRY_RUN" == "true" ]] && echo "→ dry run  : yes (no git changes)"
echo ""

# Ensure we're on main and the tree is not in a broken state
BRANCH=$(git rev-parse --abbrev-ref HEAD)
if [[ "$BRANCH" != "main" ]]; then
    echo "✗ not on main (currently on ${BRANCH})" >&2
    exit 1
fi

# ── Tests ─────────────────────────────────────────────────────────────────────
echo "→ running lib tests (release)…"
cargo test --lib --release --quiet
echo "✓ lib tests pass"

echo "→ running integration tests (release)…"
cargo test --tests --release --quiet
echo "✓ integration tests pass"

echo "→ verifying release binary…"
cargo build --release --frozen --quiet
echo "✓ release binary ok"

ENDPOINT="${OLLAMA_ENDPOINT:-http://localhost:11434}"
echo "→ running gauntlet (qwen3.5:2b-q4_K_M @ ${ENDPOINT})…"
./target/release/test_models "${ENDPOINT}" qwen3.5:2b-q4_K_M
echo "✓ gauntlet complete"

# ── Git ────────────────────────────────────────────────────────────────────────
if [[ "$DRY_RUN" == "true" ]]; then
    echo ""
    echo "✓ dry run complete — all checks passed, no git changes made"
    exit 0
fi

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
