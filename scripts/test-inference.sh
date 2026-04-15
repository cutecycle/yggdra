#!/bin/bash
# test-inference.sh — Test all available models for basic inference correctness
# Usage: ./scripts/test-inference.sh
#
# Tests each model for:
#   1. Plain response (returns content, stops cleanly)
#   2. Tool call format (follows instructions)
#   3. Hallucination (doesn't fabricate tool outputs)
#   4. Training artifacts (doesn't emit <|endoftext|> etc.)

set -euo pipefail

OLLAMA_ENDPOINT="${OLLAMA_ENDPOINT:-http://localhost:11434}"
PROXY_ENDPOINT="${PROXY_ENDPOINT:-http://localhost:11435}"

PASS=0
FAIL=0
SKIP=0

green()  { printf "\033[32m%s\033[0m" "$1"; }
red()    { printf "\033[31m%s\033[0m" "$1"; }
yellow() { printf "\033[33m%s\033[0m" "$1"; }

result() {
    local name="$1" status="$2" detail="$3"
    if [ "$status" = "PASS" ]; then
        printf "  %-50s %s\n" "$name" "$(green '✅ PASS')"
        PASS=$((PASS + 1))
    elif [ "$status" = "FAIL" ]; then
        printf "  %-50s %s  %s\n" "$name" "$(red '❌ FAIL')" "$detail"
        FAIL=$((FAIL + 1))
    else
        printf "  %-50s %s  %s\n" "$name" "$(yellow '⏭ SKIP')" "$detail"
        SKIP=$((SKIP + 1))
    fi
}

# Send a chat request and capture the full JSON response
chat() {
    local endpoint="$1" model="$2" system="$3" user="$4" num_predict="${5:-200}"
    local messages
    if [ -n "$system" ]; then
        messages=$(printf '[{"role":"system","content":%s},{"role":"user","content":%s}]' \
            "$(echo "$system" | python3 -c 'import sys,json; print(json.dumps(sys.stdin.read()))')" \
            "$(echo "$user" | python3 -c 'import sys,json; print(json.dumps(sys.stdin.read()))')")
    else
        messages=$(printf '[{"role":"user","content":%s}]' \
            "$(echo "$user" | python3 -c 'import sys,json; print(json.dumps(sys.stdin.read()))')")
    fi
    local body
    body=$(printf '{"model":%s,"stream":false,"options":{"num_ctx":4096,"num_predict":%d},"messages":%s}' \
        "$(echo "$model" | python3 -c 'import sys,json; print(json.dumps(sys.stdin.read().strip()))')" \
        "$num_predict" "$messages")
    curl -sf --max-time 120 "$endpoint/api/chat" -d "$body" 2>/dev/null || echo ""
}

extract() {
    python3 -c "
import sys, json
try:
    d = json.load(sys.stdin)
    field = '$1'
    if field == 'content':
        print(d.get('message',{}).get('content',''))
    elif field == 'done_reason':
        print(d.get('done_reason',''))
    elif field == 'eval_count':
        print(d.get('eval_count',''))
except:
    print('')
" 2>/dev/null
}

# ─── Test a single model ─────────────────────────────────────────────────────

test_model() {
    local endpoint="$1" model="$2" label="$3"
    echo ""
    echo "═══ $label ($model) ═══"

    # Test 1: Plain response
    local resp
    resp=$(chat "$endpoint" "$model" "" "What is 2+2? Reply with just the number.")
    if [ -z "$resp" ]; then
        result "$label/plain" "SKIP" "no response (model unavailable?)"
        result "$label/tool-call" "SKIP" "skipped"
        result "$label/hallucination" "SKIP" "skipped"
        result "$label/artifacts" "SKIP" "skipped"
        return
    fi
    local content done_reason
    content=$(echo "$resp" | extract content)
    done_reason=$(echo "$resp" | extract done_reason)

    if [ -n "$content" ] && echo "$content" | grep -q "4"; then
        result "$label/plain" "PASS" ""
    elif [ -n "$content" ]; then
        result "$label/plain" "FAIL" "content='${content:0:60}' (no '4' found)"
    else
        result "$label/plain" "FAIL" "empty content, done_reason=$done_reason"
    fi

    # Test 2: Training artifacts
    if echo "$content" | grep -qE '<\|endoftext\|>|<\|im_start\|>|<\|im_end\|>|<\|eot_id\|>'; then
        result "$label/artifacts" "FAIL" "training tokens in output"
    else
        result "$label/artifacts" "PASS" ""
    fi

    # Test 3: Tool call format
    local tool_system="You are a helpful agent.
FORMAT: [TOOL: name arg]
EXAMPLES:
[TOOL: readfile src/main.rs]"
    resp=$(chat "$endpoint" "$model" "$tool_system" "Read the file src/main.rs" 500)
    content=$(echo "$resp" | extract content)
    done_reason=$(echo "$resp" | extract done_reason)

    if echo "$content" | grep -q '\[TOOL:'; then
        result "$label/tool-call" "PASS" ""
    elif [ "$done_reason" = "length" ]; then
        result "$label/tool-call" "FAIL" "hit token limit without tool call"
    elif [ -z "$content" ]; then
        result "$label/tool-call" "FAIL" "empty content (may be in thinking field)"
    else
        result "$label/tool-call" "FAIL" "no [TOOL:] found: '${content:0:80}'"
    fi

    # Test 4: Hallucination detection
    if echo "$content" | grep -q '\[TOOL_OUTPUT:'; then
        result "$label/hallucination" "FAIL" "model fabricated tool output"
    else
        result "$label/hallucination" "PASS" ""
    fi
}

# ─── Discover and test models ────────────────────────────────────────────────

echo "🔍 Inference Test Suite — $(date)"
echo ""

# Local Ollama models
echo "── Local Ollama ($OLLAMA_ENDPOINT) ──"
if curl -sf "$OLLAMA_ENDPOINT/api/tags" >/dev/null 2>&1; then
    local_models=$(curl -sf "$OLLAMA_ENDPOINT/api/tags" | python3 -c "
import sys, json
for m in json.load(sys.stdin).get('models', []):
    print(m['name'])
" 2>/dev/null)
    if [ -z "$local_models" ]; then
        echo "  No local models found"
    else
        while IFS= read -r model; do
            test_model "$OLLAMA_ENDPOINT" "$model" "local/$model"
        done <<< "$local_models"
    fi
else
    echo "  $(yellow '⚠️  Ollama not running')"
fi

# OpenRouter proxy models
echo ""
echo "── OpenRouter Proxy ($PROXY_ENDPOINT) ──"
if curl -sf "$PROXY_ENDPOINT/api/tags" >/dev/null 2>&1; then
    proxy_models=$(curl -sf "$PROXY_ENDPOINT/api/tags" | python3 -c "
import sys, json
for m in json.load(sys.stdin).get('models', []):
    print(m['name'])
" 2>/dev/null)
    if [ -z "$proxy_models" ]; then
        echo "  No proxy models found"
    else
        while IFS= read -r model; do
            test_model "$PROXY_ENDPOINT" "$model" "proxy/$model"
        done <<< "$proxy_models"
    fi
else
    echo "  $(yellow '⚠️  Proxy not running')"
fi

# ─── Summary ──────────────────────────────────────────────────────────────────

echo ""
echo "═══════════════════════════════════════════"
printf "  $(green 'PASS'): %d   $(red 'FAIL'): %d   $(yellow 'SKIP'): %d\n" "$PASS" "$FAIL" "$SKIP"
echo "═══════════════════════════════════════════"

[ "$FAIL" -eq 0 ] && exit 0 || exit 1
