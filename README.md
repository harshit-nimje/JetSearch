<div align="center">

<img src="assets/jetsearch-banner.svg" alt="JetSearch — Find anything. Instantly." width="100%">

<br>

**A lightweight desktop file search engine, built with Rust.**

Search your filesystem with parallel indexing, ranked results, a persistent SQLite cache, and a responsive desktop search worker.

<br>

[![Build](https://github.com/harshit-nimje/JetSearch/actions/workflows/build.yml/badge.svg?branch=main)](https://github.com/harshit-nimje/JetSearch/actions/workflows/build.yml)
[![Rust](https://img.shields.io/badge/Rust-2021-CE422B?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![UI](https://img.shields.io/badge/UI-Slint-4B9CD3)](https://slint.dev/)
[![Parallelism](https://img.shields.io/badge/Parallelism-Rayon-7C5CFC)](https://github.com/rayon-rs/rayon)

[Overview](#-overview) · [Features](#-what-it-does) · [Getting started](#-getting-started) · [Search syntax](#-search-syntax) · [Roadmap](#-roadmap)

</div>

---

## ⚡ Overview

JetSearch is a native desktop utility for finding files and folders without waiting for a traditional recursive search on every query.

It builds an in-memory filesystem index, then searches that index in parallel. The interface is built with [Slint](https://slint.dev/), while [Rayon](https://github.com/rayon-rs/rayon) handles parallel work.

The project is under active development. Features below describe the current source code; performance claims and platform support are intentionally not stated as guarantees until they are benchmarked and tested.

## ✨ What it does

- **Parallel filesystem indexing** — walks directory trees using Rayon.
- **Persistent index cache** — stores file metadata in SQLite so JetSearch can load the previous index immediately on startup.
- **Incremental cache updates** — compares scans and writes only added, changed, and removed entries.
- **Non-blocking search** — runs queries on a background worker and cancels superseded searches.
- **Fast in-memory matching** — searches indexed names instead of re-reading the filesystem for each query.
- **Multi-term queries** — all included terms must match.
- **Typo-tolerant fallback** — tries ordered-character matching when a single-term search has no direct match.
- **Useful filters** — narrow results by extension or exclude unwanted terms.
- **Ranked results** — exact names and stronger name matches rank ahead of weaker matches.
- **Open or reveal** — open a result or reveal its containing location.
- **Persistent preferences** — search options are saved to a small configuration file.

## 🔎 Search syntax

JetSearch supports a compact query syntax:

| Query | Meaning |
|---|---|
| `invoice` | Find names containing “invoice” |
| `project notes` | Require both terms to match |
| `-draft` | Exclude names containing “draft” |
| `ext:pdf` | Only files with the PDF extension |
| `ext:pdf,docx` | Match either extension |
| `budget ext:xlsx -old` | Combine included terms, extension filter, and exclusion |

Search options in the Settings panel:

- **Match case** — make text matching case-sensitive.
- **Search in folder path** — allow included terms to match the parent directory path.
- **Show hidden files** — include hidden entries.
- **Show system files** — include entries marked as system files on Windows.

> **Note:** Extension filters apply to files, not directories. A query containing only exclusions (for example, `-draft`) does not produce results.

## 🧰 Built with

| Technology | Role |
|---|---|
| [Rust](https://www.rust-lang.org/) | Core application and indexing engine |
| [Slint](https://slint.dev/) | Native desktop UI |
| [Rayon](https://github.com/rayon-rs/rayon) | Data-parallel filesystem traversal and search |
| [SQLite](https://www.sqlite.org/) via [rusqlite](https://github.com/rusqlite/rusqlite) | Persistent index cache and transactional updates |

The release profile enables optimization, link-time optimization, a single codegen unit, and binary stripping.

## 🚀 Getting started

### Prerequisites

- Rust toolchain (stable)
- Cargo
- A platform supported by the current Slint/Winit dependencies

Install Rust using [rustup](https://rustup.rs/).

### Run from source

```bash
git clone https://github.com/harshit-nimje/JetSearch.git
cd JetSearch
cargo run
```

### Build an optimized binary

```bash
cargo build --release
```

The executable is written to:

- **Windows:** `target/release/jetsearch.exe`
- **Linux/macOS:** `target/release/jetsearch` (where the target builds successfully)

### Tests

```bash
cargo test
```

The repository currently includes unit tests for search filtering, ranking, path matching, and fuzzy fallback.

## 🖥️ Platform status

| Platform | Status |
|---|---|
| Windows | Primary target; CI builds and tests on Windows |
| Linux | Non-Windows code path exists; broader validation is still needed |
| macOS | Not yet validated |

On Windows, the current indexer scans fixed drives. Removable, network, and optical drives are excluded. On non-Windows systems, the current root list starts at `/`; filesystem permissions may prevent some directories from being indexed.

## 🗂️ Project structure

```text
JetSearch/
├── .github/
│   └── workflows/
│       └── build.yml       # Windows CI and tagged releases
├── assets/
│   └── jetsearch-banner.svg
├── src/
│   ├── index.rs            # Filesystem index, diffing, and query engine
│   ├── storage.rs          # SQLite persistence and incremental updates
│   ├── worker.rs           # Background, cancellable search worker
│   ├── lib.rs              # Shared library modules
│   └── main.rs             # Application state and UI integration
├── ui/
│   └── app.slint           # Desktop interface
├── Cargo.toml
├── Cargo.lock
└── build.rs
```

## 🧭 Roadmap

The following are areas for future work, not promises of current functionality:

- [ ] Add real-world indexing and query benchmarks.
- [ ] Add screenshots and short product demonstrations.
- [ ] Validate Linux and macOS builds and behavior.
- [ ] Improve indexing controls and progress visibility.
- [ ] Add configurable locations and exclusions.
- [ ] Expand automated testing and CI coverage.
- [ ] Publish signed, versioned release packages.

Have an idea or found a bug? Open an [issue](https://github.com/harshit-nimje/JetSearch/issues).

## 🤝 Contributing

Contributions, bug reports, and focused suggestions are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request.

## 📄 License

No license has been declared yet. Until a license is added, all rights remain with the copyright holder; please do not assume the code is available for reuse or redistribution.

---

<div align="center">

**JetSearch** · Built for finding things, not waiting for them.

<sub>Created by <a href="https://github.com/harshit-nimje">Harshit Nimje</a></sub>

</div>
