# Launch Optimization — Key Findings

## Current State Analysis

### Codebase Size
| Module | Lines | Purpose |
|--------|-------|---------|
| `ui.rs` | ~6540 | TUI app, rendering, state |
| `agent.rs` | ~75K | Agentic loop, tool parsing |
| `ollama.rs` | ~66K | Ollama HTTP client |
| `tools.rs` | ~53K | Tool implementations |
| `config.rs` | ~34K | Config management |
| **Total** | ~600K | Entire codebase |

**Implication:** `ui.rs` is 10x larger than typical module — likely contains initialization bloat.

---

### Dependencies
- **Direct dependencies:** ~45–50 in Cargo.toml
- **Transitive dependencies:** 200–300+ (cargo tree output)
- **Cargo.lock size:** 96K
- **Key heavy crates:**
  - `ratatui` — TUI rendering (large, but essential)
  - `tokio` — async runtime (heavy, but heavily used)
  - `reqwest` — HTTP client (async, may have overhead)
  - `serde_json` — JSON parsing (widely used)
  - `crossterm` — terminal control (lighter alternative: `termion`)

**Implication:** Dependency graph is moderate; no obvious bloat candidates yet.

---

### Build Profile
**Current (Cargo.toml defaults):**
```toml
[profile.release]
opt-level = 3       # Max optimization
lto = false         # ← NOT enabled (quick builds)
codegen-units = 16  # ← Parallel (quick builds, less optimization)
strip = false       # ← Symbols included
```

**Implication:** Build is fast (~30s?) but binary is NOT optimized for runtime.

---

## Startup Sequence (from main.rs)

Likely order:
1. Parse CLI args
2. Load/create session (`.yggdra_session_id`)
3. Load config (`.yggdra/config.json`)
4. Create `OllamaClient` — **may do HTTP handshake**
5. Initialize `KnowledgeIndex` — **may scan 135k files**
6. Create `App` struct — **ui.rs allocation**
7. Render first frame
8. Event loop

**Bottleneck candidates:** Steps 4, 5, 6 (3–5s each?)

---

## Quick Wins (Ranked by ROI)

| Rank | Action | Impact | Effort | Risk |
|------|--------|--------|--------|------|
| 🥇 | Lazy Ollama connect | 0.5–2s | 1–2h | Low |
| 🥈 | Defer knowledge index | 0.5–2s | 30min | Low |
| 🥉 | Optimize Cargo profile | 2–5s | 30min | Low |
| 4️⃣ | Move config load off critical path | 0.5–1s | 1h | Low |
| 5️⃣ | Audit + remove heavy deps | 1–5s | 2–3h | Medium |
| 6️⃣ | UI state lazy init | 1–2s | 2–3h | Medium |

---

## Recommended Action Plan

### Phase 1: Profiling (10 min) — **DO FIRST**
```bash
# Baseline measurements
time echo "q" | ./target/release/yggdra --ask
ls -lh target/release/yggdra
cargo bloat --release -n 30
```

### Phase 2: Quick Wins (1–2 h)
1. **P0.1:** Lazy Ollama connection
   - Move `OllamaClient::new()` from main.rs to first use
   - Store as `Option<OllamaClient>` in App

2. **P0.2:** Defer knowledge index
   - Check if scanned at startup
   - Move to first search command

3. **P1.2:** Optimize Cargo profile
   - Create `.cargo/config.toml` with `lto = "fat"` + `codegen-units = 1`
   - Accept +30s build time for 2–5s runtime gain

### Phase 3: Validation (30 min)
```bash
cargo test --lib              # Ensure no regressions
time echo "q" | ./target/release/yggdra --ask  # Measure new time
git commit -m "perf: optimize launch time"
```

---

## Risk Assessment

### Low Risk
- Lazy Ollama connect (just defer allocation + connection)
- Defer knowledge index (already lazy-loadable via search)
- Optimize Cargo profile (no code changes, just build config)

### Medium Risk
- Move config load (need loading UI state)
- Audit dependencies (potential breakage if removing active crates)

### High Risk (avoid unless desperate)
- Replace ratatui with custom renderer (months of work)
- Replace tokio with blocking runtime (loses async features)

---

## Success Metrics

**Target:** Startup to TUI render < 1s

**Measurement:**
```bash
time echo "q" | ./target/release/yggdra --ask
# Parse wall-clock time (real, not user+sys)
```

**Expected progression:**
- Baseline: ? (measure first)
- After P0.1–P0.3: 1–3s improvement
- After P1: +1–2s improvement
- After P2: +0.5–1s improvement
- **Total target:** < 1s from baseline

---

## Notes

- **Do not optimize without data:** Profile first, then target actual bottleneck
- **Cargo profile trade-off:** LTO + codegen-units = 1 makes *builds* slower (acceptable)
- **Test after each change:** `cargo test --lib` must pass
- **Commit incrementally:** One optimization per commit for easy rollback

