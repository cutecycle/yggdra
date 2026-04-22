# Relentless Launch Time Optimization Plan

## Current Baseline (to measure)
- [ ] Binary size: `target/release/yggdra`
- [ ] Release build time: `cargo build --release`
- [ ] Runtime startup to TUI render: measure with `time yggdra --ask`
- [ ] Ollama connect latency
- [ ] UI first-frame time

---

## Phase 1: Dependency & Compilation Optimization (HIGH IMPACT)

### 1.1 Dependency Audit & Removal
**Problem:** Each dependency adds to build time + binary size + link time

- [ ] Run `cargo bloat --release -n 50` — identify dead weight symbols
- [ ] Review all Cargo.toml direct deps — mark each as:
  - CRITICAL (Ollama, TUI, core logic)
  - OPTIONAL (notifications, knowledge index, watcher)
  - REMOVABLE (check for alternatives)
- [ ] Check for duplicate transitive deps (e.g., multiple versions of `tokio`, `serde`)
- [ ] Evaluate:
  - `crossterm` → `termion` (lighter, pure Rust)
  - `ratatui` → custom minimal renderer (radical, but possible for modal TUI)
  - `reqwest` → `ureq` (blocking, no async overhead for Ollama polling)
  - `serde_json` → `json` crate (lighter) OR keep for OpenAI compat
  - Remove unused: `notify`, `indicatif`, `chrono` if not critical
  - Conditional deps: move `rusqlite`, `notify` to `[dev-dependencies]`

**Est. impact:** 5–15s build time, 2–5MB binary

---

### 1.2 Cargo Optimization Profile
**Problem:** Default release profile is too conservative

Create `.cargo/config.toml`:
```toml
[profile.release]
opt-level = 3              # Already max, keep
lto = "fat"                # Link-time optimization (10–20% binary shrink, +build time)
codegen-units = 1          # Single codegen unit for better optimization (slower build)
strip = true               # Strip symbols (small binary, lose debuginfo)
panic = "abort"            # Smaller panic handler

# OR create a faster-build profile:
[profile.release-fast]
inherits = "release"
lto = false
codegen-units = 16
strip = false
```

**Trade-off:** `lto = "fat" + codegen-units = 1` adds ~30s to build but saves 2–3s at runtime + shrinks binary 10%.

**Est. impact:** 2–5s runtime, 10% binary shrink

---

### 1.3 Async Runtime Tuning
**Problem:** Tokio spawns threads + tasks even at startup

**Options:**
- [ ] Use `tokio::task::spawn_blocking` sparingly (Ollama polling)
- [ ] Consider single-threaded `tokio::runtime::Builder::new_current_thread()` for deterministic startup
- [ ] Delay async task spawning until *after* UI renders

**Est. impact:** 0.5–2s (if main thread blocked on async)

---

## Phase 2: Lazy Loading & Defer (MEDIUM-HIGH IMPACT)

### 2.1 Session/Config Initialization
**Problem:** `main.rs` → session creation → config load → all happens synchronously before TUI

**Options:**
- [ ] Load `.yggdra/config.json` *after* TUI first frame (show loading spinner)
- [ ] Defer knowledge index creation until first search
- [ ] Lazy-init Ollama connection (connect on first agent interaction, not startup)
- [ ] Move task.db / message.db queries off critical path

**Code locations:**
- `src/main.rs` — review `fn main()` and `App::new()`
- `src/ui.rs` — App struct init, `App::init_ui()`

**Est. impact:** 1–3s (if config/DB ops are blocking)

---

### 2.2 UI Initialization
**Problem:** `src/ui.rs` is 6540 lines; likely initializes all themes/palettes/widgets upfront

**Options:**
- [ ] Defer widget state allocation until layout is computed
- [ ] Lazy-load themes only when selected
- [ ] Don't pre-render off-screen buffers (compute on demand)
- [ ] Move `highlight.rs` syntax setup to background task

**Est. impact:** 1–2s (large module, likely bloated init)

---

### 2.3 Knowledge Index
**Problem:** `.yggdra/knowledge/` symlink may scan 135k files at startup

**Options:**
- [ ] Skip index build if `.yggdra/knowledge/.index.cache` exists (fresh < 1 hour)
- [ ] Move indexing to first `/` search command (not startup)
- [ ] Use mmap'd binary index instead of in-memory tree

**Code:** `src/knowledge_index.rs`

**Est. impact:** 0.5–2s (if full scan)

---

## Phase 3: Runtime Hotpath Optimization (MEDIUM IMPACT)

### 3.1 Message Buffer Initialization
**Problem:** `src/message.rs` + `src/msglog.rs` may allocate large buffers

**Options:**
- [ ] Use `Vec::with_capacity(1024)` instead of unbounded growth
- [ ] Ring buffer for scrollback (fixed size, O(1) append)
- [ ] Defer SQLite .db file open until first write

**Est. impact:** 0.2–0.5s

---

### 3.2 Ollama Client Creation
**Problem:** `OllamaClient::new()` in `main.rs` may do HTTP handshake

**Options:**
- [ ] Defer connection test until first inference
- [ ] Use lazy_static or `once_cell` for shared client
- [ ] Non-blocking socket setup

**Code:** `src/ollama.rs`, `src/main.rs`

**Est. impact:** 0.5–2s (depends on network latency)

---

### 3.3 Config Parsing
**Problem:** `src/config.rs` may deserialize complex JSON/TOML

**Options:**
- [ ] Pre-compile config schema validation (serde + serde_derive)
- [ ] Use `serde(deny_unknown_fields)` to fail fast
- [ ] Cache parsed config to avoid re-parse on reload

**Est. impact:** 0.1–0.3s

---

## Phase 4: Binary Size Reduction (NICE-TO-HAVE)

### 4.1 Strip Unused Code
- [ ] `cargo tree --duplicates` — merge duplicate deps
- [ ] `cargo unused-features` (nightly) — find unused feature flags
- [ ] Remove `#[cfg(test)]` blocks from release binary (cargo already does this, verify)

**Est. impact:** 5–15% binary size

---

### 4.2 Compression
- [ ] UPX binary compression (controversial, adds unpack time)
- [ ] Remove debug symbols with `strip = true` in Cargo.toml

**Est. impact:** 30–50% disk size, <0.1s unpack (if UPX used)

---

## Phase 5: Profiling & Validation

### 5.1 Profile Build Time
```bash
cargo clean
CARGO_PROFILE_RELEASE_DEBUG=true cargo build --release -Z timings
```

### 5.2 Profile Runtime
```bash
# Flamegraph (install: cargo install flamegraph)
cargo flamegraph --release -- --ask

# System trace (macOS)
instruments -t "System Trace" ./target/release/yggdra

# Time to first render
time /usr/bin/env bash -c 'echo "q" | ./target/release/yggdra --ask 2>/dev/null'
```

### 5.3 Binary Analysis
```bash
cargo bloat --release -n 50       # Top symbols
cargo tree --duplicates           # Duplicate deps
ls -lh target/release/yggdra      # Final size
```

---

## Prioritized Action List (by ROI)

| Priority | Action | Est. Impact | Effort | Notes |
|----------|--------|------------|--------|-------|
| 🔴 P0 | Profile current baseline | — | 10min | Must do first |
| 🔴 P0 | Lazy-load Ollama connection | 0.5–2s | 1–2h | Biggest low-hanging fruit |
| 🟠 P1 | Defer knowledge index | 0.5–2s | 30min | If scanning 135k files |
| 🟠 P1 | Move config load off critical path | 0.5–1s | 1h | Add loading spinner |
| 🟠 P1 | Optimize Cargo profile (lto + codegen) | 2–5s | 30min | Accept longer build time |
| 🟡 P2 | Audit + remove heavy dependencies | 1–5s | 2–3h | Review each dep ROI |
| 🟡 P2 | Defer UI state allocation | 1–2s | 2–3h | Large refactor, profile first |
| 🟡 P2 | Ring buffer for scrollback | 0.2–0.5s | 1h | Nice-to-have |
| 🔵 P3 | Strip symbols + UPX | Disk only | 30min | Doesn't help runtime |
| 🔵 P3 | Replace crossterm/ratatui | Radical | Weeks | Only if desperate |

---

## Measurement Checklist

Before optimizations:
```bash
# Baseline
cargo clean
time cargo build --release > /tmp/build_before.txt 2>&1
ls -lh target/release/yggdra
cargo bloat --release -n 30 > /tmp/bloat_before.txt
echo "q" | time ./target/release/yggdra --ask
```

After each optimization:
```bash
# Re-measure
cargo build --release
ls -lh target/release/yggdra
echo "q" | time ./target/release/yggdra --ask
```

---

## Success Criteria

- **Target:** Startup to TUI render < 1s (from current baseline)
- **Binary size:** < 50MB (from current)
- **Build time:** < 60s in release mode (from current)

---

## Notes

- **Avoid:** Micro-optimizations without data (profile first!)
- **Risk:** LTO + codegen-units = 1 slows *build* time by 30s (acceptable trade for 2–5s runtime gain)
- **Dependency swap risk:** Switching to lighter crates (e.g., `ureq` vs `reqwest`) may lose async features needed by Ollama streaming
- **Test regularly:** `cargo test --lib` after each change

