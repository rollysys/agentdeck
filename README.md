# agentdeck

Local web launcher for multi-agent coding profiles. One browser tab,
cards for every profile you've configured, click → `claude` / `codex` /
`qwen` / `hermes` spawns inside that profile's cwd with its env, and an
xterm.js terminal shows up right there.

![status](https://img.shields.io/badge/status-early-yellow)

## Why

If you juggle many small working directories (one for a data pipeline,
one for a research notebook, one for a scraper, one for a trading
helper, …) each pinned to a different agent / model, opening an iTerm
window and `cd`-ing every time adds up. `agentdeck` keeps all of them
one click away in the browser.

Read-only against agent session directories. No hooks, no daemon, no
telemetry. Binds `127.0.0.1:7860` only.

## Install

### Prebuilt binary (macOS aarch64, Linux x86_64)

```bash
curl -fsSL https://raw.githubusercontent.com/rollysys/agentdeck/main/install.sh | bash
agentdeck          # starts on http://127.0.0.1:7860
```

The script downloads the latest GitHub Release tarball, verifies its
SHA-256, and installs `agentdeck` to `~/.local/bin/` (override with
`PREFIX=...`). Other platforms — Intel Mac, Linux ARM — need the
"build from source" path below.

### From source (any platform with Rust ≥ 1.77)

```bash
cargo install --git https://github.com/rollysys/agentdeck
# or, for hacking:
git clone https://github.com/rollysys/agentdeck
cd agentdeck
cargo build --release
./target/release/agentdeck
```

### Linux server (remote dev workstation)

agentdeck binds `127.0.0.1:7860` only. To use it from your laptop's
browser when agentdeck runs on a remote server:

```bash
# on the server
agentdeck

# on your laptop, in another terminal
ssh -L 7860:127.0.0.1:7860 your-server
# then open http://127.0.0.1:7860 in your local browser
```

**Don't expose `0.0.0.0`.** `/ws/spawn` lets anyone who reaches it
launch any configured agent on the server (claude's Bash tool ≈ remote
shell). Stick to SSH tunnel or Tailscale; reverse proxy with auth +
TLS is out of scope for now.

## Configure

Create `~/.config/agentdeck/profiles.yaml`:

```yaml
profiles:
  - name: auditui
    cwd: /Users/you/code/auditui
    agent: claude          # claude | codex | qwen | hermes
    model: opus            # optional, currently displayed only
    skills: []             # optional, reserved for preload
    env: {}                # optional, extra env vars for the spawn

  - name: tushare-data
    cwd: ~/code/tushare
    agent: qwen

  - name: scratch
    cwd: /tmp
    agent: hermes
```

Rules:
- `cwd` must be absolute. Tilde is expanded (`~/foo` → `$HOME/foo`).
- Profile names must be unique.
- Relative `cwd` or duplicate names are skipped with an error visible
  in the UI; other profiles keep rendering.

## Use

Open http://127.0.0.1:7860. Each card has two buttons:

- **continue** — resumes the most recent session in that cwd
  (`claude --continue`, `hermes --continue`, `qwen --continue`,
  `codex resume --last`). No prior session in that cwd → the agent
  exits with its own error message.
- **new** — starts fresh.

A full-screen overlay opens with an xterm.js terminal wired to a pty
on the backend. Closing the panel sends SIGHUP and the agent exits.
No cross-close session persistence in this release — a browser reload
or close is the end of that agent process.

## How it's wired

```
browser                         deck (this bin, localhost:7860)
┌────────────────┐              ┌────────────────────────────────┐
│ index.html     │◀──GET /─────▶│ static html (inlined)          │
│ xterm.js       │              │                                │
│ WebSocket ─────┼──/ws/spawn──▶│ profile lookup                 │
│  binary↔pty    │              │ portable-pty openpty + spawn   │
│  text=JSON ctl │              │ std::thread ↔ tokio::mpsc      │
└────────────────┘              │ child: claude/codex/qwen/hermes│
                                └────────────────────────────────┘
```

- `GET /api/profiles` — loads & validates `~/.config/agentdeck/profiles.yaml`.
- `GET /ws/spawn?profile=<name>&mode=<new|continue>&cols=N&rows=M` —
  WebSocket. Binary frames are pty bytes both directions. Text frames
  are JSON control: client sends `{type:"resize",cols,rows}`; server
  sends `{type:"exit"}` / `{type:"error",message}`.

## Not in scope yet

- Session history per profile (pick a specific older session, not just
  "most recent").
- Skill preloading (the `skills:` field is carried through but not
  passed to the agent yet — each CLI has its own syntax).
- tmux / persistent sessions that survive ws close.
- Auth / remote access (runs on loopback only by design).

## Related

Born out of [`rollysys/auditui`](https://github.com/rollysys/auditui),
which is a read-only TUI for browsing the same agents' session
transcripts. `agentdeck` is the "launch and manage" side of the same
workflow and was spun into its own repo once the split made the
product boundaries clearer.

## License

MIT. See `LICENSE`.
