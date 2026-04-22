# Launch Optimization — Action Checklist

## 📊 BASELINE MEASUREMENTS (Do First!)

### Quick Profiling
```bash
# Time startup (read-only mode, fastest)
time echo "q" | ./target/release/yggdra --ask 2>&1

# Binary size
ls -lh target/release/yggdra

# Dependency graph
cargo tree --depth 1 | wc -l
cargo tree --duplicates

# Top symbols by size (if cargo bloat installed)
cargo bloat --release -n 30
```

### Build Profile
```bash
# Current build time
cargo clean && time cargo build --release 2>&1 | tail -5

# Profile with timings
CARGO_PROFILE_RELEASE_DEBUG=true cargo build --release -Z timings 2>&1 | grep -E "Compiling|Finished"
```

---

## 🔴 P0: LAZY LOADING (Highest ROI)

### P0.1: Ollama Connection Deferral
**File:** `src/main.rs`
**Problem:** OllamaClient created during `App::new()` — blocks startup on network

**Action:**
- [ ] Find `OllamaClient::new()` call in main.rs
- [ ] Move to *first use* (when agent spawns or model is queried)
- [ ] Store `Option<OllamaClient>` in App struct, init on demand
- [ ] Show spinner or "Connecting..." message on first inference

**Impact:** 0.5–2s saved (if network latency is slow)

---

### P0.2: Knowledge Index Deferral
**File:** `src/knowledge_index.rs` + `src/main.rs`
**Problem:** May scan `.yggdra/knowledge/` (135k files) at startup

**Action:**
- [ ] Find where `KnowledgeIndex::new()` is called
- [ ] Check if it scans filesystem immediately
- [ ] Move index build to *first search* command (lazy_static or once_cell)
- [ ] OR check for `.yggdra/knowledge/.index.cache` — skip rebuild if fresh

**Impact:** 0.5–2s saved (if full filesystem scan)

---

### P0.3: Config Load Deferral
**File:** `src/config.rs` + `src/main.rs`
**Problem:** Config loaded synchronously before TUI renders

**Action:**
- [ ] Identify `Config::load()` or similar in main.rs
- [ ] Load defaults first (fast)
- [ ] Load actual config *after* UI frame 1 renders
- [ ] Use `.yggdra/config.json` polling/watcher to reload

**Impact:** 0.5–1s saved

---

## 🟠 P1: DEPENDENCY AUDIT (Moderate ROI)

### P1.1: Identify Heavy Dependencies
```bash
cargo tree --depth 1 > /tmp/deps.txt
cargo bloat --release -n 50 > /tmp/bloat.txt
```

**Candidates to investigate:**
- `ratatui` (TUI framework) — 300KB+ of symbols?
- `crossterm` (terminal control) — could use `termion` instead?
- `tokio` (async runtime) — used heavily?
- `serde_json` (JSON parsing) — could trim features?
- `reqwest` (HTTP client) — blocking only? Could use `ureq`?

**Action:**
- [ ] Review each top-level dep in Cargo.toml
- [ ] Mark as: CRITICAL, OPTIONAL, or REMOVABLE
- [ ] For OPTIONAL: move to `[dev-dependencies]` or feature flag
- [ ] For REMOVABLE: find lighter alternative

**Impact:** 1–5s build time, 2–5MB binary

---

### P1.2: Optimize Cargo Profile
**File:** `.cargo/config.toml` (create if missing)

**Action:**
- [ ] Create/edit `.cargo/config.toml`:
```toml
[profile.release]
opt-level = 3              # Already max
lto = "fat"                # Link-time optimization
codegen-units = 1         # Single codegen = better optimization
strip = true               # Remove debug symbols
panic = "abort"            # Smaller panic handler
```

- [ ] Measure: `time cargo build --release` (expect +30s build time)
- [ ] Measure: `time echo "q" | ./target/release/yggdra --ask` (expect 2–5s faster)

**Trade-off:** Build slower (accept), runtime faster (win)

**Impact:** 2–5s runtime, 10% binary shrink

---

## 🟡 P2: HOTPATH OPTIMIZATION (Lower Priority)

### P2.1: Message Buffer Ring Buffer
**File:** `src/message.rs`
**Problem:** Unbounded Vec allocation for scrollback

**Action:**
- [ ] Review message buffer init
- [ ] Change to fixed-size ring buffer (e.g., 10k messages max)
- [ ] Use circular index instead of Vec::remove(0)

**Impact:** 0.2–0.5s

---

### P2.2: UI State Lazy Init
**File:** `src/ui.rs`
**Problem:** 6540 lines — likely allocates all widgets + themes upfront

**Action:**
- [ ] Profile: `cargo flamegraph --release -- --ask`
- [ ] Identify hot functions in ui.rs init
- [ ] Defer non-critical state (themes, off-screen buffers)

**Impact:** 1–2s (if confirmed via profiling)

---

## 🔵 P3: BINARY SHRINK (Nice-to-have)

### P3.1: Remove Unused Symbols
```bash
cargo tree --duplicates
cargo unused-features  # nightly only
```

**Action:**
- [ ] Merge duplicate dependencies
- [ ] Strip unused features from deps

**Impact:** 5–15% binary size (disk only)

---

### P3.2: Symbol Stripping
Already enabled in P1.2 (strip = true)

---

## 📈 VALIDATION CHECKLIST

After each optimization:
```bash
# 1. Tests pass
cargo test --lib

# 2. Binary still works
./target/release/yggdra --ask --help

# 3. Measure improvement
time echo "q" | ./target/release/yggdra --ask
ls -lh target/release/yggdra

# 4. Commit if good
git add -A && git commit -m "perf: optimize launch time (P0.X)"
```

---

## 🎯 SUCCESS CRITERIA

| Metric | Target | Current (baseline) |
|--------|--------|-------------------|
| Startup to TUI render | < 1s | ? |
| Binary size | < 50MB | ? |
| Build time (release) | < 60s | ? |

---

## 📋 EXECUTION ORDER

1. **Profile baseline** (10 min) — gather data
2. **P0.1–P0.3** (2–3 h) — lazy loading (biggest wins)
3. **P1.1–P1.2** (2–3 h) — deps + cargo profile
4. **P2.1–P2.2** (2–4 h) — hotpath (only if P0/P1 insufficient)
5. **Validate** (30 min) — measure final vs baseline

**Estimated total:** 4–8 hours for 2–10s improvement

