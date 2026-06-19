# agent.md — Developer & Agent Guide

> This file tells human developers and AI coding agents **how** to work on this project.
> For **what** the project is and does, see [`spec.md`](./spec.md).
> Always keep `spec.md` in context when making architectural decisions.

---

## Language & Toolchain

- **Language**: Rust (stable, latest stable toolchain via `rustup`)
- **Rust edition**: 2024
- **Package manager**: Cargo
- **Formatter**: `rustfmt` (run before committing)
- **Linter**: `clippy` (zero warnings policy on new code)

```bash
rustup update stable
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
```

---

## Repository Layout

```
ask-ubuntu-docs/
├── spec.md                   # What the app is (keep in context)
├── agent.md                  # This file
├── Cargo.toml                # Single-crate package
├── Cargo.lock
├── cli-system-prompt.md      # System prompt embedded at compile time
├── product-prompts.toml      # Per-product system prompt additions, embedded at compile time
├── src/
│   ├── main.rs               # Entry point; clap CLI definition (subcommands: chat, gui)
│   ├── cli.rs                # Interactive terminal chat loop
│   ├── gui.rs                # GTK4 / libadwaita graphical interface
│   ├── conversation.rs       # Conversation history + RAG-augmented message assembly
│   ├── llm.rs                # LLM backends: OllamaClient, CopilotClient, LlmClient enum
│   ├── vectordb.rs           # LanceDB RAG store: load, embed, hybrid search
│   ├── markdown.rs           # Markdown → Pango markup conversion for GTK labels
│   └── prompts.rs            # Loads product-prompts.toml; returns per-product system prompt
└── snap/
    ├── snapcraft.yaml        # Snap packaging (base: core26, strict confinement)
    ├── hooks/
    │   ├── install           # Copies index.lance from $SNAP to $SNAP_COMMON on install
    │   └── post-refresh      # Same, runs after snap refresh
    └── gui/                  # Desktop entry / GUI launcher assets
```

---

## Build & Run

```bash
# Build
cargo build

# Run the graphical interface with GitHub Copilot
cargo run gui --copilot

# Run the terminal chat with GitHub Copilot
cargo run chat --copilot

# Run the graphical interface with a local Ollama model
cargo run gui --model deepseek-r1:1.5b

# Run the terminal chat with a local Ollama model
cargo run chat --model deepseek-r1:1.5b
```

For dev builds, the app expects the LanceDB index at `target/index.lance`. Download a pre-built index from the [ubuntu-docs-indexer releases](https://github.com/msuchane/ubuntu-docs-indexer/releases), unpack it, and place it at `target/index.lance`. Or set `UBUNTU_HELP_INDEX_PATH` to any path.

---

## CLI Flags & Environment Variables

All flags are defined in `src/main.rs` via `clap`.

| Flag | Env var | Default | Description |
|---|---|---|---|
| `--model <name>` | `MODEL` | `deepseek-r1:1.5b` (Ollama) / `gpt-4o-mini` (Copilot) | Model name |
| `--copilot` | — | off | Use GitHub Models API instead of Ollama |
| `--ollama-url <url>` | `OLLAMA_URL` | `http://localhost:11434` | Ollama server base URL |

`--model` and `--copilot` are global flags (work with both `chat` and `gui` subcommands).
`--ollama-url` is subcommand-level (present on both `chat` and `gui`).

---

## Index Path Resolution

`src/vectordb.rs` resolves the LanceDB index path in this order:

1. `UBUNTU_HELP_INDEX_PATH` env var — explicit override (dev, CI, snap)
2. `$SNAP_USER_DATA/index.lance` — written by the install/post-refresh hook
3. `$SNAP/index.lance` — read-only fallback baked into the snap
4. `target/index.lance` — dev build fallback (relative to `CARGO_MANIFEST_DIR`)

---

## GitHub Token Resolution (Copilot mode)

`src/llm.rs` (`CopilotClient::create`) tries three sources in order:

1. `COPILOT_TOKEN` env var — use directly
2. GNOME Keyring / KWallet via Secret Service (`service=gh:github.com`) — works in snaps with the `password-manager-service` plug
3. `gh auth token` CLI — last resort; requires the GitHub CLI to be installed and authenticated

---

## Key Source Files

`main.rs`
: Parses CLI args with `clap`, dispatches to `cli::run` or `gui::run`. Both share the same `LlmClient` and `RagStore`.

`conversation.rs`
: `Conversation` struct: stores bare user/assistant turn history (no RAG chunks). Builds the augmented user message (docs context + question) for each LLM call. Prepends the last assistant reply to the RAG query to handle follow-up questions.

`llm.rs`
: `OllamaClient` (NDJSON streaming), `CopilotClient` (SSE streaming via `models.github.ai`). Both expose `chat()` (stdout) and `chat_streaming()` (callback). Unified `LlmClient` enum dispatches to either.

`vectordb.rs`
: `RagStore`: loads a LanceDB table, initialises the BGE-small-en-v1.5 embedder, provides `embed()` and `search_with_vec()` (hybrid BM25 + vector, top 12 chunks).

`gui.rs`
: GTK4 + libadwaita chat UI. Product selector dropdown maps to per-product system prompts. Streams tokens into the chat view via `chat_streaming()` and an async channel.

`cli.rs`
: Line-by-line stdin loop. Shows a spinner until the first token, then streams tokens to stdout.

`prompts.rs`
: Loads `product-prompts.toml` at compile time via `include_str!`. `get_prompt(product)` returns the system-prompt addition for the selected product.

`markdown.rs`
: Converts Markdown to Pango markup for `gtk::Label::set_markup()`.

---

## GUI ↔ CLI Feature Parity

The GUI (`src/gui.rs`) and the CLI (`src/cli.rs`) share the same backend logic (RAG, LLM, conversation history). **Whenever you change the behaviour of one interface, apply the equivalent change to the other** — unless the change is genuinely interface-specific (e.g. a widget layout tweak or a terminal escape code).

Concretely:
- A bug fixed in the CLI chat loop should also be fixed in the GUI message-send handler, and vice versa.
- A new feature (e.g. RAG query contextualisation, history trimming, model selection) must land in both.
- If the change truly cannot be applied to the other interface, leave a `// TODO(parity):` comment explaining why.

---

## Snap Packaging Notes

- Base: `core26`
- Confinement: `strict`
- The `rag-index` snap part downloads the pre-built LanceDB index from the latest [ubuntu-docs-indexer](https://github.com/msuchane/ubuntu-docs-indexer) release at snap build time and stages it at `$SNAP/index.lance`.
- The `install` and `post-refresh` hooks copy the index to `$SNAP_COMMON/index.lance` (writable) so LanceDB can open it.
- The snap sets `UBUNTU_HELP_INDEX_PATH=$SNAP_COMMON/index.lance` in the app environment.
- Key plugs: `network`, `home`, `password-manager-service` (GNOME Keyring), `desktop`, `wayland`, `x11`, `opengl`.

---

## Testing

```bash
# Unit tests
cargo test --all
```

There are currently no integration tests. When adding them, gate them behind an env var (e.g. `INTEGRATION_TESTS=1`) since they require Ollama or network access.

---

## Code Style

- Use `anyhow::Result` for error propagation; avoid `.unwrap()` outside tests
- Always comment code to explain *why*, not just *what*; assume the reader has basic Rust knowledge. Add at least one comment per code block.
- Keep each module focused on one responsibility (see source file list above)
- Prefer iterators and `?`-propagation over explicit loops and match chains
- Do not add new dependencies without checking they compile in a snap-constrained context

---

## System Package Dependencies

This project requires system packages for building. **Agents do not have `sudo` access.** If a build fails due to a missing system package, stop and ask the user to install it:

```bash
sudo apt-get install -y <package-name>
```

Known required packages:
- `libgtk-4-dev` — GTK4 development headers
- `libadwaita-1-dev` — libadwaita development headers
- `pkg-config` — used by build scripts to locate system libraries
- `libssl-dev` — TLS support for reqwest
- `g++` — provides libstdc++ required by LanceDB/ORT native deps
- `protobuf-compiler` + `libprotobuf-dev` — required by `lance-encoding` (transitive LanceDB dep)
