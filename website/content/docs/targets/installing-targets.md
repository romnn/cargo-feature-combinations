---
title: Installing targets
weight: 3
---

# Installing missing targets

By default `cargo fc` does not mutate the Rust toolchain. If a configured target's `rustup` component isn't installed, the build for that target fails as it normally would.

## Opt in per invocation

```bash
cargo fc check --install-missing-targets
```

This installs the missing target components with `rustup` before running. It's an explicit opt-in because it may mutate the toolchain and use the network. When a `+toolchain` override is present, the same override is passed to `rustup`, so components land in the right toolchain.

## Opt in for the workspace

```toml
[workspace.metadata.cargo-fc]
install_missing_targets = true
```

## CI recommendation

In CI, preinstalling the targets in your toolchain setup step is usually more reproducible and cache-friendly than `--install-missing-targets`:

```yaml
- uses: dtolnay/rust-toolchain@stable
  with:
    targets: x86_64-unknown-linux-gnu, wasm32-unknown-unknown
- uses: romnn/cargo-feature-combinations@main
- run: cargo fc check
```

See [Continuous integration]({{< relref "../ci/_index.md" >}}).
