# gh6 — GitHub Social Graph Explorer

> Crawl the GitHub follow graph, discover connections, find bridges.  
> A six-degrees-of-separation inspired social graph explorer for GitHub.

[![Crates.io][crates-badge]][crates-url]
[![MIT][mit-badge]][mit-url]

[中文](README.md)

[crates-badge]: https://img.shields.io/badge/crates.io-0.1.0-orange
[crates-url]: https://crates.io/crates/gh6
[mit-badge]: https://img.shields.io/badge/license-MIT-blue
[mit-url]: https://opensource.org/licenses/MIT

---

## Overview

`gh6` is a **client-server** tool that crawls GitHub's social graph by following
the "following" edges outward from a seed user — like a breadth-first search
over the GitHub follow network. It stores everything in a local SQLite database
and provides a rich CLI for analysis: shortest paths, common connections,
community detection, bridge discovery, and user recommendations.

The name is a nod to *six degrees of separation* — `gh6` explores just how
close any two GitHub users really are.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│              gh6d (daemon) + gh6 (client)                    │
│                                                             │
│  ┌──────────────┐          ┌──────────────────────────────┐ │
│  │    gh6d      │ ──write──→  ~/.local/share/gh6/gh6.db   │ │
│  │  (launchd /  │          │         SQLite               │ │
│  │   systemd)   │          └──────────────┬───────────────┘ │
│  └──────┬───────┘                         │  direct read    │
│         │  Unix socket                    │                 │
│         │  ~/.local/share/gh6/gh6.sock    │                 │
│  ┌──────┴──────────┐          ┌──────────┴───────────────┐ │
│  │ gh6 run         │          │  gh6 analyze {subcommand} │ │
│  │ gh6 pause       │          │  gh6 export               │ │
│  │ gh6 status      │          └──────────────────────────┘ │
│  └─────────────────┘                                       │
└─────────────────────────────────────────────────────────────┘
```

### Components

| Component | Description |
|-----------|-------------|
| **gh6d** | Background daemon managed by launchd (macOS) / systemd (Linux). Starts idle, waits for commands via Unix socket. |
| **gh6** | CLI client that talks to gh6d over a Unix socket. Sends commands (`run`, `pause`, `status`) and performs offline analysis. |

## Quick Start

### Prerequisites

- Rust (edition 2024) — `rustup` recommended
- A [GitHub personal access token](https://github.com/settings/tokens) with `read:user` scope
  - Set via `GITHUB_TOKEN` environment variable, or
  - `gh auth login` (the tool will fall back to `gh auth token`)

### Install

```bash
# Build and install both binaries
cargo install --path .

# Or use the install script (macOS launchd integration)
./install.sh
```

### Start the Daemon

```bash
# If you used install.sh — launchd manages it automatically
# Otherwise, start manually:
gh6d --seed <your-github-login> --workers 3
```

### Crawl the Graph

```bash
gh6 run         # start/resume crawling
gh6 status      # view progress
gh6 status --watch       # real-time updates
gh6 status --watch --progress  # live status bar
gh6 pause       # pause (daemon stays alive)
```

## CLI Reference

### Daemon

```
gh6d [OPTIONS]

Options:
  --seed <LOGIN>    Seed user to start crawling from (defaults to authenticated user)
  --workers <N>     Number of parallel crawl workers (default: 3)
```

### Client Commands

#### Crawl Control

| Command | Description |
|---------|-------------|
| `gh6 run` | Start or resume crawling |
| `gh6 pause` | Pause the crawl (daemon stays alive) |
| `gh6 status` | Show crawl progress snapshot |
| `gh6 status --watch` | Real-time event stream |
| `gh6 status --watch --progress` | Live status bar at bottom |

#### Graph Analysis

| Command | Description |
|---------|-------------|
| `gh6 analyze route <LOGIN> [--from <LOGIN>] [--limit N] [--fuzzy]` | Find shortest path(s) between two users |
| `gh6 analyze common <LOGIN> <LOGIN> [--limit N]` | Mutual follows / fans |
| `gh6 analyze user <LOGIN> [--detail]` | User profile + social graph |
| `gh6 analyze suggest <LOGIN> [--limit N]` | User recommendations based on common follows |
| `gh6 analyze bridges [--limit N]` | Find bridge nodes connecting communities |
| `gh6 analyze communities [--limit N] [--user <LOGIN>]` | Community detection |
| `gh6 analyze stats` | Database overview |
| `gh6 export <FILE>` | Export graph to JSON |

All analysis commands support `--json` for machine-readable output.

## Feature Details

### Crawler

- **BFS over GitHub follow graph** — starts from a seed user and follows the `following` edges outward, degree by degree
- **Hub deferral** — users with >5000 followings are crawled last to avoid exhausting the API rate limit
- **Parallel workers** — configurable concurrency (default: 3) with atomic scope claiming
- **Resumable** — crawl state persists in SQLite; pause and resume at any time
- **Graceful shutdown** — respects `SIGINT`/`SIGTERM` mid-page
- **Rate-limit aware** — adapts to GitHub API rate limits automatically

### Analysis

- **Shortest path** — BFS-based pathfinding with optional fuzzy login matching
- **Common connections** — intersection of follows/followers between two users
- **User recommendations** — collaborative filtering via common follow connections
- **Bridge detection** — identifies users who connect otherwise-disconnected communities
- **Community detection** — simple community discovery in the social graph

### Database

- **SQLite** — local storage at `~/.local/share/gh6/gh6.db`
- **Schema layers** — stable layer (users, edges) + extension layer (profiles, history, crawl state)
- **Edge history** — tracks changes over time for temporal analysis
- **Config** — key-value config table persisted alongside crawl data

## Service Management

### macOS (launchd)

```bash
# Install (builds binaries, loads launchd plist)
./install.sh

# Uninstall
./install.sh uninstall

# Manual control
launchctl bootstrap gui/$UID ~/Library/LaunchAgents/com.gh6.daemon.plist
launchctl bootout  gui/$UID ~/Library/LaunchAgents/com.gh6.daemon.plist
```

### Linux (systemd — user-level)

```bash
# Copy the service file
cp gh6d.service ~/.config/systemd/user/

# Enable & start
systemctl --user daemon-reload
systemctl --user enable --now gh6d

# View logs
journalctl --user -u gh6d -f
```
## Project Structure

```
src/
├── main.rs         # gh6 CLI entry point (clap commands)
├── lib.rs          # module declarations
├── types.rs        # shared type definitions
├── db.rs           # SQLite database layer
├── github.rs       # GitHub API client
├── server.rs       # daemon (socket listener, crawl loop)
├── display.rs      # terminal output primitives & views
├── analyze.rs      # graph analysis queries
├── crawlers/
│   └── mod.rs      # BFS crawl logic (FollowCrawler)
└── bin/
    └── gh6d.rs     # daemon entry point

migrations/
└── 001_init.sql    # database schema
```

## Related Documents

- [DESIGN.md](DESIGN.md) — detailed architecture and design decisions
- [TODO.md](TODO.md) — development progress and roadmap
- [AGENTS.md](AGENTS.md) — project conventions for AI-assisted development

## License

MIT
