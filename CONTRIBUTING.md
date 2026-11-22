# Contributing to Markdown Studio

Thank you for considering contributing to **Markdown Studio**! Contributions of all kinds—bug reports, feature suggestions, documentation improvements, and pull requests—are warmly welcome.

---

## Ways to Contribute

- **Report bugs** using the [Bug Report template](.github/ISSUE_TEMPLATE/bug_report.md).
- **Suggest features** using the [Feature Request template](.github/ISSUE_TEMPLATE/feature_request.md).
- **Improve documentation** (guides, keyboard shortcut references, code comments).
- **Submit pull requests** for fixes, performance improvements, cross-platform polish, or new features.

Before starting work on a major change, consider opening an issue first so we can discuss design and scope.

---

## Development Setup

### Prerequisites

- **Rust** (stable toolchain via [rustup](https://rustup.rs/), edition 2024)

#### Windows
- MSVC build tools (Visual Studio Build Tools with C++ desktop workload).
- Optional: Windows SDK (`rc.exe`) so `build.rs` embeds the application icon and `VERSIONINFO` into `markdown_studio.exe`.

#### Linux (Ubuntu / Debian)
`eframe` / `egui` and `rfd` require X11/Wayland, OpenGL/xkb, and GTK3 development headers:

```bash
sudo apt-get update
sudo apt-get install -y \
  libxcb-render0-dev \
  libxcb-shape0-dev \
  libxcb-xfixes0-dev \
  libxkbcommon-dev \
  libssl-dev \
  libgtk-3-dev \
  libatk1.0-dev \
  libgdk-pixbuf-2.0-dev \
  libpango1.0-dev \
  libcairo2-dev \
  fonts-dejavu-core \
  fonts-liberation
```

#### macOS
- Xcode Command Line Tools (`xcode-select --install`).

---

## Running Checks, Tests & Benchmarks

```bash
# Run all unit and integration tests
cargo test --no-fail-fast

# Run Clippy across all targets
cargo clippy --all-targets -- -D warnings

# Check formatting
cargo fmt --check

# Run Criterion benchmarks (10 MB / 1 MB stress suite)
cargo bench
```

See [ARCHITECTURE.md](ARCHITECTURE.md) for an overview of the module layout, data flow, and core invariants (such as $O(\log n)$ `ropey::Rope` edits, viewport virtualization, and zero-loss GFM round-trips).

---

## Pull Request Guidelines

1. Fork the repository and create a branch from `main`.
2. Keep changes focused and add unit or integration tests for new behavior or bug fixes.
3. Ensure `cargo test` and `cargo clippy --all-targets -- -D warnings` pass cleanly.
4. Keep commits clear and descriptive.

---

## License

Markdown Studio is licensed under the **GNU Affero General Public License v3.0 or later (`AGPL-3.0-or-later`)**. By submitting a contribution, you agree that your contributions will be licensed under the same terms (see [LICENSE](LICENSE)).
