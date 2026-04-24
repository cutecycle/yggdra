# 🌳 yggdra

Yggdra is an airgapped agentic TUI — a Rust app that connects to
[Ollama](https://ollama.ai), gives your model a toolbox (ripgrep, git, python,
file editing, even nested sub-agents), and lets it operate on your
filesystem. No cloud. No API keys. No phoning home. Just you, a terminal, and
a local model.

```
┌──────────────────────────────── yggdra ─────────────────────────────────┐
│ 🌷 session a1b2c3  ·  qwen3:8b  ·  build mode  ·  3 tasks remaining  │
│─────────────────────────────────────────────────────────────────────────│
│ user: find all the TODO comments and summarize them                     │
│ assistant: on it — searching with rg first…                             │
│ [TOOL_OUTPUT: rg = src/ui.rs:42: // TODO: dark mode toggle]            │
│ assistant: found 7 TODOs across 4 files. here's the summary…           │
│─────────────────────────────────────────────────────────────────────────│
│ >                                                                       │
└─────────────────────────────────────────────────────────────────────────┘
```

~6k lines of Rust · 4.8 MB binary · <50 MB RAM · zero network dependencies

---

## Why?

Most agentic coding tools need the cloud. Yggdra doesn't. It's built for the
scenario where you *can't* — or *won't* — send your code to someone else's
servers. Plug in an Ollama instance (local or on your tailnet), point it at a
project, and let it work. The TUI is clean, sessions persist, and the model can
spawn sub-agents to parallelize work.

---

## Quick start

**You need:** [Ollama](https://ollama.ai) running somewhere reachable, and
Rust 1.70+ to build.

```bash
git clone https://github.com/cutecycle/yggdra.git
cd yggdra
cargo build --release
./target/release/yggdra          # or: make install → ~/.local/bin/yggdra
```

First run? Pull a model and go:

```bash
ollama pull qwen3:8b             # or whatever you like
yggdra                           # that's it
```

---

## Four modes

| Flag | Mode | What it does |
|------|------|-------------|
| `--ask` | **Ask** | Read-only. Answers questions, no autonomous actions, can't touch your files. |
| `--plan` | **Plan** (default) | Interactive. Agent proposes; you drive. |
| `--build` | **Build** | Autonomous. Agent kicks itself to keep working until you stop it. |
| `--one` | **One** | Like Build, but stops with an OS notification when the task is complete (`[DONE]` or no tool calls in a turn). |

Mode persists across sessions (saved to `~/.yggdra/config.json` or
`.yggdra/config.json`), so you only set it once. Cycle in-app: Plan → Build →
One → Ask.

---

## The toolbox

Available tools (all local, no network):

| Tool | What it does |
|------|-------------|
| **rg** | Ripgrep search across files |
| **exec** | Run local commands (PATH-resolved, no network binaries) |
| **shell** | Run a shell command (sh -c wrapper) |
| **setfile** | Write entire file — creates if missing, git-tracked |
| **patchfile** | Line-range replacement (targeted patch without full-file overwrite) |
| **commit** | Stage and commit with git |
| **python** | Execute Python snippets |
| **ruste** | Compile and run Rust snippets |
| **spawn** | Spawn a child sub-agent (up to 10 levels deep) |

Tool calls default to JSON (OpenWebUI-style). Legacy `<|tool>…<|end_tool>` and
`[TOOL: …]` formats are still parsed for backward compatibility.

Results come back as `[TOOL_OUTPUT: name = ...]` and the model keeps going.

### Sub-agents

The model can spawn child agents to handle subtasks in parallel — up to 10
levels deep. Each sub-agent gets the same tools (minus the ability to spawn
further agents, to prevent infinite recursion).

---

## Sessions & storage

Every directory gets its own session. Walk into a project, launch yggdra, and
your conversation is right where you left it.

```
.yggdra_session_id               ← marker file (add to .gitignore)

~/.yggdra/
└── sessions/<uuid>/
    ├── messages.db               ← SQLite — fast, transactional
    └── metadata.json             ← model, timestamps, etc.
```

Open two terminals in the same project? Both instances share the same SQLite
DB and sync via polling — you'll see messages appear in both windows.

---

## The knowledge base

Symlink `.yggdra/knowledge` to a local docs folder and the model can search
it. The default setup points at `~/source/repos/offlinebase` — 135,000+ files
across 73 categories (Rust docs, Godot tutorials, spacecraft systems, whatever
you've curated).

The model searches it with `rg` like any other directory. No indexing server,
no vector DB, just rg.

---

## Knowledge gaps

After every response, yggdra quietly asks the model: *"what did you wish you
knew?"* The answers get logged to `.yggdra/gaps`. Over time, you build a map
of what your local model struggles with — handy for knowing what to add to
your knowledge base.

---

## Configuration

| Variable | Default | What it does |
|----------|---------|-------------|
| `OLLAMA_ENDPOINT` | `http://localhost:11434` | Where your Ollama lives |
| `OLLAMA_MODEL` | auto-detect | Which model to use |

Config files (`~/.yggdra/config.json` for global, `.yggdra/config.json` for
per-project) override env vars and persist mode/model/endpoint between runs.
Both are watched live — edit and the running TUI picks it up.

```bash
export OLLAMA_ENDPOINT=http://10.0.0.5:11434   # tailnet box
export OLLAMA_MODEL=qwen3:8b
yggdra --build
```

### Global agent instructions

Create `~/AGENTS.md` to define your personal preferences, persona, or
constraints that apply across **every** project:

```markdown
# My global instructions

- Prefer short, direct answers
- Always write tests for Rust code
- My name is Nina — address me by name
```

When yggdra starts, it reads `~/AGENTS.md` first, then appends the
project-local `AGENTS.md` (if any) after a `# --- project AGENTS.md ---`
separator. Both files are watched live — edits are picked up without restart.

### OpenAI-compatible endpoints (OpenRouter, etc.)

Yggdra speaks Ollama's API by default but works with any OpenAI-compatible
server. Point `endpoint` in `~/.yggdra/config.json` at a local proxy — for
example, an OpenRouter proxy on `http://localhost:11435` — and yggdra will
talk to it like it's Ollama. Useful for hosted models when you're not strictly
airgapped.

---

## Slash commands

- `/help`, `/models`, `/quit`
- `/one` — switch to One mode for a single autonomous task
- `/abort` — kill stuck streams, async tasks, and in-flight tool execution
- `/shell CMD` — run a shell command inline without going through the agent
- `/test_notification` — fire a test OS notification to verify your setup

### macOS notifications

yggdra uses `osascript` for native notifications (notify-rust silently fails on
unbundled CLIs). To allow them:

1. **System Settings → Notifications → Script Editor**
2. Enable **Allow Notifications**

Run `/test_notification` inside yggdra to confirm.

---

## Nice touches

- **Native notifications** — OS notifications on new session and model response.
- **Adaptive theming** — detects light/dark terminal and picks colors
  accordingly. Solarized-inspired palette.
- **Structured logging** — every message written to
  `.yggdra/log/YYYY/MM/DD/HHMM/SS-role.md` for full auditability.
- **Task tracking** — built-in SQLite-backed task graph with checkpoints and
  dependency tracking. The model manages its own todo list.
- **Steering directives** — inject system-level constraints (be concise, output
  JSON, etc.) that get prepended to every prompt.

---

## Building & testing

```bash
cargo build --release            # optimized binary (LTO, stripped)
cargo test --lib                 # run the test suite — keep it green
make install                     # copy to ~/.local/bin/
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the full dev guide and
[ARCHITECTURE.md](ARCHITECTURE.md) for the deep dive.

---

## Vendored dependencies

All crates are committed to `vendor/` via `cargo vendor`. Building requires
**zero network access** — `cargo build --release` resolves everything locally.

- **Offline by design**: perfect for air-gapped machines; no crates.io reach
  required at build time.
- **Reproducible**: the exact dependency tree is in the repo — no registry
  surprises, no yanked crates, no `Cargo.lock` drift between machines.
- **CI-friendly**: pipelines without internet access build cleanly.

This is wired up in `.cargo/config.toml`:

```toml
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
```

To update a dependency: run `cargo update` then `cargo vendor` and commit the
changes to `vendor/`.

---

## Troubleshooting

**"Ollama is offline"** — Start it (`ollama serve`) or point
`OLLAMA_ENDPOINT` at the right address.

**"Model not found"** — `ollama pull <model>` or type `/models` inside yggdra
to see what's available.

**Slow responses** — That's your GPU talking, not yggdra. Try a smaller model
or check `ollama ps`.

**Session weirdness** — Delete `.yggdra_session_id` in the project dir to
start fresh.

---

## License

MIT
