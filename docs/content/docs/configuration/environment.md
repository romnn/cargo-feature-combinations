---
title: Child-process environment
weight: 6
---

# Child-process environment

Use `env` to set or remove environment variables for the Cargo process spawned
for each matrix cell. It is available everywhere `driver` is available, so the
environment can vary by workspace, package, target, command, or any combination
of those scopes.

```toml
[workspace.metadata.cargo-fc]
env = { add = { RUST_BACKTRACE = "1" }, remove = ["OPENSSL_DIR"] }

[workspace.metadata.cargo-fc.target.'cfg(not(target_os = "linux"))']
env = { remove = ["ORT_LIB_PATH"] }

[package.metadata.cargo-fc.subcommands.check]
env = { add = { ORT_STRATEGY = "download" } }
```

Only matrix-cell Cargo invocations receive the resolved patch. Cargo-fc's own
metadata queries, driver probes, rustup installs, and target detection continue
to use the ambient environment.

## Patch grammar

`env` always uses explicit patch operations:

```toml
env = { override = { BASE = "value" }, remove = ["OLD", "AMBIENT_SECRET"], add = { NEW = "value", BASE = "narrower-value" } }
```

- `override` resets the accumulated **patch** to exactly that map. It does not
  clear the ambient process environment.
- `remove` removes variables from the child even when they came from the
  ambient environment.
- `add` sets or replaces variables. Operations apply in the order override,
  remove, add, so `add` wins when the same name is also removed.

A bare map is deliberately rejected:

```toml
# Invalid: use env.add.RUST_BACKTRACE instead.
env = { RUST_BACKTRACE = "1" }
```

Variable names must be nonempty and may not contain `=` or NUL. Values are
literal strings, may be empty, and may not contain NUL. Cargo-fc does not
interpolate `${VAR}` or resolve relative paths.

## Precedence and inheritance

Environment patches follow the normal [override model]({{< relref
"override-model.md" >}}), broadest to narrowest:

1. workspace base
2. workspace subcommand
3. workspace target
4. workspace target subcommand
5. package base
6. package subcommand
7. package target
8. package target subcommand
9. `--unset-env`, then `--env`

A narrower `add` for the same name replaces the broader value. A narrower
`remove` cancels a broader addition and also removes an ambient value.

`inherit = false` discards all broader cargo-fc environment patches before the
current scope is applied. It still does not clear the ambient environment:

```toml
[package.metadata.cargo-fc.subcommands.test]
inherit = false
env = { add = { RUST_BACKTRACE = "full" } }
```

When multiple matching target sections contribute to one layer, removals are
combined. Additions for the same name must have equal values, and `override`
maps must be equal; conflicting values are an error.

## One-off CLI overrides

Use repeatable CLI options to override the environment resolved from config:

```sh
cargo fc check --unset-env ORT_LIB_PATH --env ORT_STRATEGY=system
```

`--env KEY=VALUE` splits on the first `=`, accepts an empty value (`KEY=`), and
uses the last occurrence when a key is repeated. All `--unset-env` removals are
applied first, then all `--env` additions. This lets a one-off command replace
or restore values introduced dynamically by narrower config scopes.

For an invocation-wide value that does not need to override cargo-fc config,
the shell remains equivalent and often simpler:

```sh
env RUST_BACKTRACE=1 cargo fc test
env -u OPENSSL_DIR cargo fc check
```

## Interaction with cargo-fc variables

Cargo-fc's own child-process injections consult the resolved child view instead
of blindly overwriting it:

| Variable or behavior | Interaction |
|---|---|
| `CARGO_DRIVER` | Cargo-fc sets the resolved driver unless `env` or the CLI explicitly sets or removes this variable. |
| `CARGO_TERM_COLOR`, `FORCE_COLOR` | Color forcing fills only variables absent from the resolved child view. |
| `NO_COLOR` | A resolved value disables forcing; removing it can re-enable forcing on a terminal. |
| `RUSTFLAGS`, `CARGO_ENCODED_RUSTFLAGS` | `--errors-only` appends `-Awarnings` to the resolved value. |

Setting `CARGO` does **not** change which executable cargo-fc spawns. Driver
resolution and ambient `CARGO` choose the executable before the child patch is
applied; the configured `CARGO` is visible only to nested processes started by
that child.

## Difference from Cargo's `[env]`

Cargo's `[env]` table configures Cargo itself and supports fields such as
`force` and `relative`. Cargo-fc's `env` is different: it patches only the
per-matrix-cell child process, configured additions always override ambient
values, and `force`/`relative` are not supported.

## Values, output, and platform notes

Cargo-fc never includes environment values in diagnostics, debug output, or
`cargo fc matrix` JSON. Conflict errors name only the variable. CLI values can
still be visible in shell history or the operating system's process listing,
and manifest values remain readable from `Cargo.toml`; this redaction is not a
secret store.

- Setting `PATH` can affect executable lookup differently across platforms.
- Cargo-fc compares names case-sensitively. Windows environments are
  case-insensitive, and the operating system resolves that nuance at spawn.
- Aggregate mode groups targets only when their resolved driver and environment
  are equal; differing values produce separate aggregate invocations.
