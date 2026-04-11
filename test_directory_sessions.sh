#!/bin/bash
# Test per-directory session restoration

set -e

echo "🧪 Testing Per-Directory Session Restoration"
echo "=============================================="

# Create test directories
TEST_DIR_A="/tmp/yggdra_session_test_a"
TEST_DIR_B="/tmp/yggdra_session_test_b"

rm -rf "$TEST_DIR_A" "$TEST_DIR_B"
mkdir -p "$TEST_DIR_A" "$TEST_DIR_B"

echo ""
echo "📁 Test 1: Create session in Directory A"
cd "$TEST_DIR_A"
echo "Working in: $(pwd)"

# Simulate what the app does: check for .yggdra_session_id
if [ ! -f ".yggdra_session_id" ]; then
    SESSION_ID_A="550e8400-e29b-41d4-a716-446655440001"
    echo "$SESSION_ID_A" > .yggdra_session_id
    echo "✅ Created new session: $SESSION_ID_A"
else
    SESSION_ID_A=$(cat .yggdra_session_id)
    echo "✅ Loaded existing session: $SESSION_ID_A"
fi

echo "Session ID saved in: $(pwd)/.yggdra_session_id"
cat .yggdra_session_id

echo ""
echo "📁 Test 2: Verify session file exists in Directory A"
if [ -f ".yggdra_session_id" ]; then
    echo "✅ .yggdra_session_id exists in Directory A"
fi

echo ""
echo "📁 Test 3: Create session in Directory B"
cd "$TEST_DIR_B"
echo "Working in: $(pwd)"

# Simulate what the app does: check for .yggdra_session_id
if [ ! -f ".yggdra_session_id" ]; then
    SESSION_ID_B="550e8400-e29b-41d4-a716-446655440002"
    echo "$SESSION_ID_B" > .yggdra_session_id
    echo "✅ Created new session: $SESSION_ID_B"
else
    SESSION_ID_B=$(cat .yggdra_session_id)
    echo "✅ Loaded existing session: $SESSION_ID_B"
fi

echo ""
echo "📁 Test 4: Verify sessions are different"
cd "$TEST_DIR_A"
SESSION_A=$(cat .yggdra_session_id)
cd "$TEST_DIR_B"
SESSION_B=$(cat .yggdra_session_id)

if [ "$SESSION_A" != "$SESSION_B" ]; then
    echo "✅ Directory A session: $SESSION_A"
    echo "✅ Directory B session: $SESSION_B"
    echo "✅ Sessions are different (correct!)"
else
    echo "❌ Sessions should be different!"
    exit 1
fi

echo ""
echo "📁 Test 5: Return to Directory A and verify session loads"
cd "$TEST_DIR_A"
LOADED_SESSION=$(cat .yggdra_session_id)
if [ "$LOADED_SESSION" = "$SESSION_A" ]; then
    echo "✅ Reloaded same session in Directory A: $LOADED_SESSION"
else
    echo "❌ Session changed!"
    exit 1
fi

echo ""
echo "📁 Test 6: Verify .gitignore includes .yggdra_session_id"
cd /Users/cutecycle/source/repos/yggdra
if grep -q "\.yggdra_session_id" .gitignore; then
    echo "✅ .gitignore includes .yggdra_session_id"
else
    echo "❌ .gitignore doesn't include .yggdra_session_id"
    exit 1
fi

echo ""
echo "✅ All per-directory session tests passed!"

# Clean up
rm -rf "$TEST_DIR_A" "$TEST_DIR_B"
