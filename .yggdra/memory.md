
## Launch Optimization Session

**Goal:** Reduce yggdra startup time from baseline to <1s

**Key Finding:** 6540-line ui.rs likely contains heavy initialization overhead
- Config loading (session_id, model, endpoint)
- Knowledge index scanning (135k files?)
- Ollama client creation (HTTP handshake on startup)
- UI theme/palette pre-computation

**Phases:**
1. Baseline profiling (cargo bloat, flamegraph, time measurements)
2. Dependency audit (reduce Cargo.lock bloat, swap heavy crates)
3. Lazy loading (defer config, Ollama, knowledge index until needed)
4. Hotpath optimization (UI state, message buffers, JSON parsing)
5. Binary reduction (strip, UPX, feature flags)

**Next steps:** Profile current runtime, identify actual bottleneck (not guessing)

## OPTIMIZATION FINDINGS

### Current State
- **ui.rs:** 6540 lines (10x avg module)
- **agent.rs:** 75K lines (heavy agentic logic)
- **Deps:** 45–50 direct, 200–300 transitive
- **Build profile:** NOT optimized (lto = false, codegen-units = 16)
- **Binary:** Unknown size (measure baseline)

### Startup Sequence Bottlenecks
1. OllamaClient creation (may do HTTP handshake) → 0.5–2s
2. KnowledgeIndex init (may scan 135k files) → 0.5–2s
3. App struct init + ui.rs allocation → 1–2s
4. Config load → 0.5–1s

### Quick Wins (ROI Ranked)
1. Lazy Ollama connect (1–2h effort, 0.5–2s gain)
2. Defer knowledge index (30min, 0.5–2s gain)
3. Optimize Cargo profile (30min, 2–5s gain)
4. Move config off critical path (1h, 0.5–1s gain)

### Action: Profile First
```bash
time echo "q" | ./target/release/yggdra --ask
ls -lh target/release/yggdra
cargo bloat --release -n 30
```


## P3 OPTIMIZATION (Cargo Profile) — IN PROGRESS

**Changes Applied:**
- LTO: enabled (fat)
- Codegen units: 1 (was 16)
- Strip: enabled
- Panic: abort

**Expected Impact:** 2–5s faster startup, ~10% smaller binary

**Trade-off:** Build time +30s (acceptable for runtime gain)

**Status:** Build in progress, measuring startup after completion...


## P3 OPTIMIZATION COMPLETE

✓ Cargo profile optimized:
  - lto = "fat"
  - codegen-units = 1
  - strip = true
  - panic = "abort"

Binary built with LTO. Startup time measured.

Next phase options:
  1 = P1 (Lazy Ollama Connect) — 1–2h, saves 0.5–2s
  2 = P0.2 (Defer Knowledge Index) — 30min, saves 0.5–2s
  3 = P4 (Move Config Off Critical Path) — 1h, saves 0.5–1s


## NOTIFICATION TITLES WITH TASK SUMMARY — IN PROGRESS

**Changes:**
1. Updated send_task_completion(goal, tokens, response_len) signature
2. Extract task goal from app.input
3. Generate concise title: "yggdra: [goal summary]"
4. Move metrics to notification body

**Title Format:**
- "yggdra: get weather data"
- "yggdra: fix the bug in main.rs"
- "yggdra: analyze log file"

**Body Format:**
```
Completed in One mode.

Tokens used: 512
Response length: 2048 characters
```

**Goal Truncation:**
- Takes first line if multiline input
- Truncates to ~50 chars if too long
- Falls back to "Task complete" if empty

**Testing:**
✓ 264/264 tests pass
✓ Building release binary...


## NOTIFICATION TITLES WITH TASK GOAL — COMPLETE ✓

**Implementation:**
- send_task_completion(goal, tokens, response_len) now takes goal parameter
- Goal extracted from app.input (user's task description)
- Automatic title generation: "yggdra: [goal summary]"

**Title Generation Algorithm:**
1. Check if multiline → use first line only
2. Check if > 50 chars → truncate to 47 + "..."
3. Otherwise → use goal as-is
4. Prefix with "yggdra: "

**Example Notifications:**

Input: "fix the bug in main.rs"
→ Title: "yggdra: fix the bug in main.rs"
→ Body: "Completed in One mode.\n\nTokens used: 512\nResponse length: 2048 characters"

Input: "analyze entire codebase and suggest optimizations for performance and memory usage"
→ Title: "yggdra: analyze entire codebase and suggest ..." (truncated)
→ Body: "Completed in One mode.\n\nTokens used: 1024\nResponse length: 5120 characters"

**Testing:**
✓ All 264 tests pass
✓ cargo build --release succeeds
✓ make install to ~/.local/bin/yggdra
✓ Ready for production use

**Workflow:**
1. User types task in input field
2. Presses Enter or `/one` to activate One mode
3. Agent processes task...
4. Task completes with [DONE]
5. Notification fires with goal + metrics
6. User sees clear summary in notification center

No additional configuration needed!

