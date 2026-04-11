# 🌳 yggdra

**Your local LLM just learned how to use tools.**

Yggdra is an airgapped agentic TUI — a tiny Rust app that connects to
[Ollama](https://ollama.ai), gives your model a toolbox (ripgrep, git, python,
file editing, even nested sub-agents), and lets it *do things* on your
filesystem. No cloud. No API keys. No phoning home. Just you, a terminal, and
a surprisingly capable local model.

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
project, and let it work.

It's also just… nice? The TUI is clean, sessions persist, and the model can
spawn sub-agents to parallelize work. It feels like pair programming with
someone who never gets tired and never judges your variable names.

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

## Three modes

| Flag | Mode | What it does |
|------|------|-------------|
| `--plan` | **Plan** (default) | Interactive back-and-forth. You drive, the model assists. |
| `--build` | **Build** | Autonomous. Reads `AGENTS.md` and gets to work — no hand-holding. |
| `--ask` | **Ask** | Read-only. The model can look but can't touch your files. |

Mode persists across sessions (saved to `.yggdra/config.json`), so you only
set it once.

---

## The toolbox

The model doesn't just talk — it acts. Six built-in tools, all local:

| Tool | What it does |
|------|-------------|
| **rg** | Ripgrep search across files |
| **spawn** | Run local commands (ls, cat, etc. — no network binaries allowed) |
| **editfile** | Read and write files |
| **commit** | Stage and commit with git |
| **python** | Execute Python snippets |
| **ruste** | Compile and run Rust snippets |

Tools use a Qwen/Gemma-style format that the model emits naturally:

```
<|tool>rg<|tool_sep>TODO<|tool_sep>src/<|end_tool>
```

Results come back as `[TOOL_OUTPUT: rg = ...]` and the model keeps going.

### Sub-agents

The model can spawn child agents to handle subtasks in parallel — up to 10
levels deep. Each sub-agent gets the same tools (minus the ability to spawn
further agents, to prevent recursion nightmares).

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
no vector DB, just grep and vibes.

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

```bash
export OLLAMA_ENDPOINT=http://10.0.0.5:11434   # tailnet box
export OLLAMA_MODEL=qwen3:8b
yggdra --build
```

---

## Nice touches

- **Battery-aware indexing** — knowledge base indexing slows down when you're
  on battery power. Your laptop won't hate you.
- **Native notifications** — 🌷 on new session, 🌻 on model response. Cute
  *and* functional.
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

MIT — do whatever you want.

---

*Built for the space between "I want an AI coding assistant" and "I refuse to
upload my code." That space is bigger than people think.*
