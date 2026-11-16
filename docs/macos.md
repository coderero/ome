# Running OME on macOS

This guide covers running the whole simulated market on a Mac (Apple Silicon or Intel), with or without Docker.

> OME has not been tested on macOS yet. Everything below follows from how the code and scripts are built: no Linux-specific system calls and no OpenSSL. One known script issue (bash 3.2) has a workaround in step 1. Please report anything that behaves differently.

## What runs where

| Part | How it runs | Needed? |
|---|---|---|
| Exchange, clearing house, brokers, venues, newsroom, simctl, trading agents | native Rust programs started by `scripts/demo.sh` | always |
| Desktop trading app (`ome-retail`) | native window, drawn with Metal | optional |
| PostgreSQL and Redis | Docker, or Homebrew | only for recording (`OME_RECORD=1`) and 1-minute chart history |
| Ollama (local LLM for news) | native app (recommended on Mac), or Docker | optional; without it the newsroom uses its event catalogue |

Docker is never required. On a Mac, Ollama should run natively: Docker on macOS cannot reach the Apple GPU, so Ollama inside Docker runs on the CPU only.

## 1. One-time setup

```bash
xcode-select --install                 # compiler and linker
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"   # Homebrew, if missing
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh     # Rust
brew install bash                      # bash 4 or newer for the scripts
```

Open a new terminal, then check:

```bash
rustc --version      # a recent stable toolchain; the code uses the 2024 edition (Rust 1.88 or newer)
bash --version       # must say 4.x or 5.x, not 3.2
```

**Why the newer bash:** macOS ships bash 3.2 at `/bin/bash`. `scripts/seed.sh` uses `${symbol,,}` (lower-casing), which needs bash 4, so a fresh demo would stop at seeding with `bad substitution`. The scripts start with `#!/usr/bin/env bash`, so they use the first `bash` on your `PATH`. On Apple Silicon, Homebrew installs it at `/opt/homebrew/bin/bash`, which comes first once Homebrew's shell setup has run (`eval "$(/opt/homebrew/bin/brew shellenv)"` in `~/.zprofile`).

## 2. Get the code and build

```bash
git clone git@github.com:coderero/ome.git
cd ome
git checkout updates
cargo build --release --workspace
```

The first build takes a few minutes. `scripts/demo.sh` also builds, so you can skip this step.

## 3. Choose how to run

### A. Minimal: no Docker, no LLM

```bash
OME_WRITER=templates scripts/demo.sh --fresh
```

The full market starts: exchange, clearing house, four brokers, wholesaler, dark pool, newsroom, simctl and about 80 trading agents. News comes from the built-in event catalogue with template text.

### B. LLM news without Docker (recommended on Mac)

Install and start Ollama natively; it uses the Apple GPU through Metal:

```bash
brew install ollama          # or install the Ollama app from ollama.com
ollama serve                 # leave running in its own terminal (the app does this for you)
ollama pull qwen2.5:1.5b     # small and fast
scripts/demo.sh --fresh
```

The demo checks `http://127.0.0.1:11434` first. When native Ollama answers, it is used and Docker is not touched. With Ollama running, the local model invents most company and sector stories, and the newsroom checks every story before it can move any price.

Bigger models write better stories. Pick by unified memory:

| Mac memory | Model |
|---|---|
| 8 GB | `qwen2.5:1.5b` |
| 16 GB | `qwen2.5:7b` |
| 32 GB or more | `qwen2.5:7b` or `qwen2.5:14b` (better, slower) |

```bash
ollama pull qwen2.5:7b
OME_LLM_MODEL=qwen2.5:7b scripts/demo.sh --fresh
```

### C. With Docker (Docker Desktop, OrbStack or Colima)

PostgreSQL and Redis for recording and chart history:

```bash
scripts/db.sh up
OME_RECORD=1 scripts/demo.sh --fresh
```

If Docker is running and no native Ollama answers, `scripts/demo.sh` starts Ollama in Docker by itself (`scripts/llm.sh up`). That works, but on a Mac it runs on the CPU and is slow. Start native Ollama first (option B), or set `OME_WRITER=templates` to skip the LLM.

### D. Recording without Docker (Homebrew PostgreSQL and Redis)

```bash
brew install postgresql@16 redis
brew services start postgresql@16
brew services start redis
export PATH="$(brew --prefix postgresql@16)/bin:$PATH"
psql postgres -c "CREATE ROLE ome LOGIN PASSWORD 'ome';"
psql postgres -c "CREATE DATABASE ome OWNER ome;"
OME_RECORD=1 scripts/demo.sh --fresh
```

The recorder creates its tables on first start. The defaults match this setup:
- `OME_POSTGRES=postgres://ome:ome@127.0.0.1/ome`
- `OME_REDIS=redis://127.0.0.1/`

`scripts/db.sh psql` and `scripts/db.sh redis` call Docker, so use the tools directly instead:

```bash
psql -h 127.0.0.1 -U ome ome -c "select venue, count(*) from trades group by venue"
redis-cli GET ome:book:ACME
```

## 4. The desktop trading app

With the market running:

```bash
cargo run --release -p ome-retail
```

Log in as `alice` / `alice-pw`. The broker address is `127.0.0.1:7101`, under Connection. For a scripted start:

```bash
cargo run --release -p ome-retail -- --user alice --secret alice-pw --interval 60
```

The window draws with Metal. Where things are stored:
- broker address, login ID and settings: `~/.config/ome/retail.conf` (the password is never stored);
- layouts: the app's eframe storage under `~/Library/Application Support`.

## 5. Everyday commands

```bash
scripts/stop.sh                                   # stop everything (journals are kept)
scripts/demo.sh                                   # start again and resume from the journals
scripts/demo.sh --fresh                           # wipe the journals in OME_DATA and start a new market

./target/release/ome exchange instruments         # symbols, prices, halts and pauses
./target/release/ome exchange vol                 # observed price moves
./target/release/ome news status                  # economy, companies, LLM writer statistics
./target/release/ome news list 20                 # recent stories
./target/release/ome sim status                   # agent groups, fills, equity
./target/release/ome broker --addr 127.0.0.1:7101 --user alice --secret alice-pw buy ACME mkt 5
```

Logs and journals live in `data/` (or `OME_DATA`), one `.log` and one `.journal` per service.

## 6. Settings (environment variables)

| Variable | Default | Meaning |
|---|---|---|
| `OME_DATA` | `data` | where journals, logs and pid files go; point it elsewhere for a separate market |
| `OME_WRITER` | `auto` | `auto` uses Ollama when it answers, `llm` always tries it, `templates` never uses it |
| `OME_LLM` | `http://127.0.0.1:11434` | Ollama address |
| `OME_LLM_MODEL` | `qwen2.5:1.5b` | model for invented and rewritten stories |
| `OME_RECORD` | `0` | `1` records the market into PostgreSQL and Redis |
| `OME_POSTGRES` / `OME_REDIS` | local `ome` database / local Redis | recorder and chart history connections |
| `OME_TRADING_DAY` | `5 45 5 5` | seconds of pre-open, open, closing auction and closed; `off` keeps the market always open |
| `OME_REGIME` | `us` | `us` allows internal crosses, the wholesaler and the dark pool; `india` sends every order to the exchange |
| `OME_NEWS_PER_DAY` | `1` | stories per company per trading day |
| `OME_QUARTER_DAYS` | `20` | trading days between earnings reports |
| `OME_AGENT_SCALE` | `1` | multiply every agent group's size |

## 7. Ports

All services listen on `127.0.0.1` only.

| Port | Service |
|---|---|
| 7000 | exchange |
| 7101 / 7102 / 7103 / 7104 | retail, prime, DMA and liquidity-provider brokers |
| 7200 | clearing house |
| 7301 / 7302 | wholesaler and dark pool |
| 7400 | simulation control (simctl) |
| 7500 | newsroom |
| 5432 / 6379 | PostgreSQL / Redis (recording only) |
| 11434 | Ollama |

## 8. Troubleshooting

| Symptom | Fix |
|---|---|
| `bad substitution` while seeding | the scripts ran under bash 3.2; install bash 4+ (step 1) and open a new terminal |
| `already running` or `address already in use` | run `scripts/stop.sh`; if a port is still taken, `lsof -i :7000` shows who holds it |
| no price movement after start | give agents a minute to buy their starting inventory; check `data/agents.log` |
| news is all template text | Ollama is not answering; check `curl http://127.0.0.1:11434/api/tags` and `ome news status` |
| news is slow or mostly from the catalogue | the model is too big for the machine, or Ollama runs in Docker on the CPU; use native Ollama and a smaller model |
| the app window opens but stays on the login screen | the market is not running, or the broker address under Connection is wrong |
| strange state after changing `config/companies.conf` or `config/roster.conf` | start a new market with `scripts/demo.sh --fresh` |
