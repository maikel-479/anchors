# Anchors

Anchors is a small tool I made to find and highlight “anchor” tags in code comments — things like `NOTE:`, `TODO(name):`, `FIXME:`, or `=NOTE=` — plus a bit of inline emphasis inside comments (`**bold**`, `*italic*`, `_underlined_`, `` `code` ``).

It comes as a CLI and an LSP server (`canc`), so you can scan files in the terminal/CI and also get highlighting, diagnostics, symbols/outline, and completions in editors that support it (I mainly use it with Zed).

## Getting started

Build from source:

```sh
cargo build --release
# binary: target/release/canc
```

Or install with Cargo:

```sh
cargo install canc
```

Basic usage:

```sh
canc anchors path/to/project   # scan files
canc serve                     # LSP over stdio
canc init-zed                  # print Zed semantic token rules
canc config check [PATH]
canc config print [PATH]
```

For Zed, point it at the `canc` binary (or use the bundled extension) and add the semantic token rules from `canc init-zed` to your settings. There’s an optional `anchors.toml` for configuring tags/behavior (global: `~/.config/canc/anchors.toml`, or per-project by walking up from the file).

## Contributing

This project is open to contributions — it’s not perfect, not yet — so if you spot issues, have ideas, or want to improve something, feel free to open an issue or PR.
