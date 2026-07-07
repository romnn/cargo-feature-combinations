# Crate Review: Robustness, UX, and Architecture (July 2026)

Full review of cargo-feature-combinations (v0.2.3, post config-override-chain
redesign). Findings are grouped into correctness bugs (`BUG-*`), robustness and
UX issues (`UX-*`), and architecture/simplification proposals (`ARCH-*`), each
with a concrete implementation suggestion. A suggested milestone ordering is at
the end.

Verification commands (run after every change):

```sh
task test        # or: cargo test
task lint        # clippy with the workspace lint set + ast-grep rules
```

## What is already good (do not regress)

- The config precedence chain (`config/scope.rs` + `config/resolve.rs` +
  `config/patch.rs`) is implemented once and reused for flags, driver,
  targets, exclude_packages, and features. This was the point of the previous
  redesign and it landed well.
- Planning is cleanly staged: candidate selection → target plans
  (`plan/targets.rs`) → execution plans (`plan/execution.rs`) → runner. Each
  stage owns its data; execution borrows nothing from temporary configs.
- Trait seams (`TargetEnvironment`, `CfgEvaluator`, `TargetInstaller`) make
  planning unit-testable without spawning `rustc`/`rustup`. Keep this pattern.
- Test coverage is strong and behavior-focused; the schema key-disjointness
  test and the flag-macro subset test are exactly the right kind of drift
  guards. Extend that idea (see ARCH-5) rather than replacing it.

---

## 1. Correctness bugs

### BUG-1: bare-word `matrix` / `version` and `--pretty` are matched anywhere in argv

`src/cli.rs:671-699` uses `args.get_all("matrix", false)` (and `"version"`,
`"--pretty"`), which matches the literal token at *any* position before `--`,
including as the value of a forwarded flag or a positional argument of the
cargo subcommand:

- `cargo fc test version` → prints `cargo-fc v0.2.3` instead of running tests
  filtered by `version`.
- `cargo fc test --features matrix` → runs `cargo fc matrix` with a dangling
  `--features` forwarded.
- `cargo fc nextest run --pretty` → `--pretty` is silently eaten.

**Fix:** only treat `matrix`/`version` as a cargo-fc command when they are the
*subcommand token*, reusing the existing skip logic:
`cli::subcommand_token_index(&args) == Some(idx) && args[idx] == "matrix"`.
Only consume `--pretty` when matrix mode is active (and only before `--`).
This is a small targeted change; the fuller parser restructure is ARCH-4.

### BUG-2: `--errors-only` RUSTFLAGS composition is defeated by ambient flags

`src/runner.rs:628-636` prepends:

```rust
cmd.env("RUSTFLAGS", format!("-Awarnings {}", std::env::var("RUSTFLAGS").unwrap_or_default()));
```

rustc lint-level flags are last-wins, so a user/CI environment with
`RUSTFLAGS="-Dwarnings"` silently overrides `-Awarnings` and `--errors-only`
does nothing. Two adjacent problems:

1. If `CARGO_ENCODED_RUSTFLAGS` is set, cargo ignores `RUSTFLAGS` entirely and
   the flag is a silent no-op.
2. Setting the `RUSTFLAGS` env var at all makes cargo ignore
   `[target.<triple>] rustflags` from `.cargo/config.toml` — for cross targets
   that carry link args there, `--errors-only` can *break the build*.

**Fix:**
- Append instead of prepend: `format!("{existing} -Awarnings")` so the
  explicit cargo-fc flag wins over ambient env.
- When `CARGO_ENCODED_RUSTFLAGS` is present, extend that variable instead
  (unit separator `\x1f` joined) and leave `RUSTFLAGS` alone.
- Document (help text + README) that `--errors-only` sets `RUSTFLAGS` and
  therefore shadows config-file `rustflags`; consider a warning when a
  matching `[target.<triple>].rustflags`-style config is plausible (probably
  overkill — documentation is enough).

### BUG-3: unknown `-p/--package` / `--exclude-package` names succeed silently

`src/lib.rs:497-523` (`select_candidate_packages`) retains on name membership
without checking that every requested name matched. `cargo fc check -p typo`
produces an empty plan set, prints `Finished 0 feature combination for 0
packages in 0.00s`, and exits 0 — a typo in CI silently disables the check.
Cargo itself errors on unknown package specs.

**Fix:** after filtering, error if any name in `options.packages` (and warn if
any in `options.exclude_packages`) matched no workspace member. Include the
list of available package names in the error (or a closest-match hint).
Also warn when config `exclude_packages` values match nothing (lower value,
optional).

### BUG-4: config type errors surface as raw untagged-enum serde messages

`src/package.rs:101` and `src/workspace.rs:71` do
`serde_json::from_value(value.clone())?` with no context. Because every
set-like field is an untagged enum, a common typo like
`exclude_features = "gpu"` (string instead of array) fails with:

> data did not match any variant of untagged enum StringSetPatch

— no key name, no section, no package. The validation pass (`validate.rs`)
already walks the raw JSON but only checks key *names* and scopes, not value
shapes.

**Fix (two layers):**
1. Cheap: wrap the `from_value` calls with
   `.wrap_err_with(|| format!("invalid [{section}] configuration in package `{name}`"))`.
2. Better: teach `validate_scope` value shapes for patch-typed keys — must be
   an array, or an object whose keys ⊆ {`override`, `add`, `remove`} with
   array values; booleans for flag keys; string for `driver`. Then serde
   failures become unreachable in practice and every error names the exact
   key and section. This is a natural extension of the existing
   `SettingKind` table.

### BUG-5: repeated cargo-fc value flags are first-wins, not last-wins

`ArgumentParser::get_all` (`src/cli.rs:66-94`) reverses matches so spans can
be drained back-to-front — but the extraction loops then *assign* in that
reversed order, so for `cargo fc --driver a --driver b check` the effective
driver is `a`. CLI convention (and cargo's) is last-wins. Same for
`--manifest-path`.

**Fix:** iterate matches for side-effect-assignment in original order (drain
separately), or keep only the last match's value. Add a test.

### BUG-6: inline-value parsing uses `trim_start_matches` instead of `strip_prefix`

`src/cli.rs:84-86`: `key.trim_start_matches(&format!("{arg}="))` removes the
prefix *repeatedly* (`--driver=--driver=x` → `x`). Pathological input, but
`strip_prefix` is strictly correct and cheaper (also avoids the double
`format!`). One-line fix.

### BUG-7: misleading validation message for deprecated keys in target/subcommand scope

`src/config/validate.rs:263-266`: `denylist` inside
`[package.metadata.cargo-fc.target.'cfg(unix)']` errors with *"feature-matrix
settings are per-package and are not valid in workspace scope"* — but the
scope *is* package scope; the real rule is "deprecated spellings are only
accepted at the package base; use the new name". Give `DeprecatedFeature` its
own error text pointing at the modern key.

---

## 2. Robustness / UX / DX

### UX-1: child output color is forced even when stdout is not a terminal

`src/runner.rs:63-66` (`force_color`) unconditionally sets
`CARGO_TERM_COLOR=always` + `FORCE_COLOR=1`, while cargo-fc's own output uses
`ColorChoice::Auto`. Piping `cargo fc check > log` yields a file full of ANSI
escapes from the child but plain text from cargo-fc.

**Fix:** gate on `std::io::stdout().is_terminal()` (plus honor `NO_COLOR` /
`CARGO_TERM_COLOR` if the user already set them — don't clobber an explicit
user value). Keep forcing when interactive, since that is the whole point of
the env vars.

### UX-2: child stderr is streamed to cargo-fc's stdout

`capture_stderr` (`src/runner.rs:208-239`) tees the child's *stderr* into
cargo-fc's *stdout*. Consequences: `cargo fc check 2>/dev/null` does not
silence compiler output; `cargo fc check | grep` sees diagnostics; the
stdout/stderr convention every cargo user expects is inverted.

**Decision needed:** either (a) stream child stderr to stderr (convention;
keep the summary on stdout), or (b) keep as-is and document it. (a) is the
right long-term answer; the summary/matrix on stdout and all
diagnostics/progress on stderr matches cargo itself. Note `matrix` output is
already clean stdout — this change only affects run mode. If (a), the same
`StandardStream::stderr(Auto)` handle should be used for headers and the tee.

### UX-3: `cargo fc` with no subcommand runs the whole matrix of no-op cargo calls

With no cargo subcommand, planning still runs and each combination spawns
plain `cargo` (feature args suppressed via
`PreparedInvocationArgs::is_missing_command`, `src/invocation_args.rs:91`) —
N useless subprocesses, confusing output. `cargo fc -h` behaves even worse:
`-h` is not extracted (only `--help` is), so it forwards `cargo -h
--no-default-features ...` per combination.

**Fix:** in `run()`, if the prepared command has no subcommand token and no
forwarded args, print help and exit 0 (or a short "missing cargo subcommand"
error, exit 2). Handle `-h` alongside `--help`. Then delete the
`is_missing_command` workaround from `invocation_args` (one less special
case).

### UX-4: implicitly selected `cargo-zigbuild` driver hard-fails the run when missing

When any planned target is non-host, `cross_target_default_driver`
(`src/lib.rs:699-717`) picks `cargo-zigbuild` implicitly. If it is not
installed, the spawn fails, a good warning prints, but the run then *errors
out entirely* (`spawn_cargo_command` returns Err) — for a driver the user
never asked for.

**Fix:** when the driver came from the built-in default (not config/CLI),
probe availability at finalize time (attempt `cargo-zigbuild --version`
once), and degrade to plain cargo with the existing warning. Explicitly
configured drivers should keep failing hard — the user asked for them.

### UX-5: `--workspace` is stripped but `--exclude` is forwarded

`src/cli.rs:749-758` strips `--workspace` (correct, cargo-fc emulates
workspace iteration), but cargo's companion flag `--exclude <spec>` is left
in the forwarded args; without `--workspace`, cargo rejects it:
`--exclude can only be used together with --workspace`. So the natural
`cargo fc check --workspace --exclude foo` fails on every combination.

**Fix:** extract `--exclude <name>` into `options.exclude_packages` in the
same pass that strips `--workspace` (it is the same emulation). Document both
in the help text.

### UX-6: `--help` interception hides cargo's own help

Any `--help` before `--` becomes cargo-fc help, so `cargo fc clippy --help`
prints cargo-fc's help rather than clippy's. **Fix:** treat `--help`/`-h` as
cargo-fc help only when it appears *before* the subcommand token (or when
there is no subcommand); otherwise forward it. Falls out naturally from the
positional parsing fix (BUG-1 / ARCH-4).

### UX-7: `VERBOSE` env var is too generic

`src/cli.rs:584-586` reads `VERBOSE`, which CI images commonly export
globally, silently enabling verbose headers. **Fix:** prefer
`CARGO_FC_VERBOSE`; keep `VERBOSE` as a fallback for one release with a note
in the changelog, or drop it (it is undocumented today). Document whichever
survives in the help text.

### UX-8: silent failures while loading cargo alias config

`src/cargo_alias.rs:196-201` ignores unreadable/unparsable
`.cargo/config.toml` files (`let Ok(..) else continue`). A TOML syntax error
silently disables alias expansion, so `cargo fc lint` may plan targets with
the wrong capability while `cargo lint` itself errors loudly. **Fix:**
`print_warning!` once per failing path (parse errors only — a missing file is
normal).

### UX-9: `matrix` no-op flag notes are incomplete

`note_matrix_noop_flags` (`src/lib.rs:645-660`) covers
`--install-missing-targets`, `--aggregate-targets`, `--driver`, but
`--diagnostics-only`, `--dedupe`, `--summary-only`, `--fail-fast`,
`--errors-only`, `--pedantic`, `--show-pruned` are equally ignored by matrix
output and stay silent. **Fix:** collect ignored run-only CLI flags
generically (compare `options.flags` fields against a run-only list) and emit
one combined note: `--summary-only, --fail-fast have no effect for matrix
output`. Kills the ad-hoc list.

### UX-10: help text gaps

`HELP_TEXT` (`src/cli.rs:392-570`) does not mention: `-p/--package`,
`--manifest-path`, `--packages-only`, the `--summary`/`--silent`/`--dedup`
aliases, the `version` command, the `CARGO`/`CARGO_DRIVER` env vars, or the
verbose env var. The TODO already plans `embedme` for the README — do that at
the same time so README and `--help` cannot drift.

### UX-11: minor output polish

- `print_summary` pluralizes on `> 1`, so zero prints “0 feature
  combination” (`src/runner.rs:355-372`). Use `!= 1`.
- `capture_stderr`'s `eprintln!("ERROR: failed to redirect stderr")`
  (`src/runner.rs:225`) should use `print_warning!` for consistent styling.
- Broken pipe: `println!` panics on EPIPE (`cargo fc check | head`). Consider
  routing summary printing through the `StandardStream` handle and tolerating
  `ErrorKind::BrokenPipe` (exit 0/141), or note it as a known limitation.
- `--exclude-package` values are trimmed, `--package` values are not
  (`src/cli.rs:649-657`). Trim both.

### UX-12: only the first `--target` is honored in planning

`parse_cli_target` (`src/target.rs:130-151`) returns the first occurrence.
Cargo accepts repeated `--target` flags (multi-target builds); cargo-fc plans
only the first triple while forwarding all of them, so summaries misattribute.
Rare; either support a list in `TargetExpansion::Explicit` or reject repeated
`--target` with a clear error. The error is the cheap, honest option.

---

## 3. Architecture and simplification

The config chain is in good shape after the redesign; the remaining
architectural debt is concentrated in three places: `lib.rs` doing policy work
that belongs to phases, `runner.rs` duplicating its two execution loops, and
`cli.rs` mixing parsing with the command registry. Everything below is
behavior-preserving except where it fixes a bug above.

### ARCH-1: slim the orchestrator (`lib.rs`)

`run()` currently owns driver finalization (`finalize_plan_drivers`,
`cross_target_default_driver`, `normalize_driver`, ~80 lines), execution-mode
policy (`resolve_execution_mode`, ~80 lines), and three warning policies.
These are phase policies, not orchestration:

- Move driver finalization into a new `src/driver.rs` (or into
  `plan::execution` as a finalize pass). It operates on
  `ExecutionPlanSet` + `TargetEnvironment` only. UX-4's availability probe
  lands there naturally.
- Move `resolve_execution_mode` into `runner.rs` — it returns
  `runner::TargetExecutionMode` and reasons entirely about runner semantics
  (aggregate constraints, `run` incompatibility, pruned summaries).
- Move `warn_if_configured_targets_ignored` / `warn_ignored_diagnostics_config`
  / `note_matrix_noop_flags` next to the phases that produce their inputs
  (target planning and execution-plan building respectively), or a small
  `src/hints.rs`.

Target state: `lib.rs` = metadata keys, the two print macros (see ARCH-6),
`run()` as a readable page of phase calls, and `prepare_cargo_command`.
Everything else lives in a named phase module. Also delete the dead
`WARN_UNKNOWN_SUBCOMMAND` block (`src/lib.rs:111`, `440-452`) and the
unreachable `Some(Command::Help | Command::Version) => Ok(None)` match arm
(help/version already returned earlier).

### ARCH-2: unify the two runner execution loops

`execute_serial` and `execute_aggregate` (`src/runner.rs:797-943`) duplicate
the run-record-maybe-stop-print-summary skeleton. Model the run as a flat
step list built up front:

```rust
enum Step<'a> {
    Run(Invocation<'a>),
    /// After a (package, target) block in serial mode: append pruned rows.
    AppendPruned { pkg_start_marker: (), package: &'a str, target: SummaryTarget, pruned: &'a [PrunedCombination] },
}
```

Serial mode emits `Run..Run, AppendPruned` per package-target; aggregate mode
emits only `Run` steps (it already falls back when pruned summaries are
shown). One executor loop then owns progress numbering, fail-fast, and the
final summary. This removes ~80 lines, and guarantees the two modes cannot
drift in fail-fast/summary behavior.

### ARCH-3: move command-capability lookup out of `cli.rs`, kill the `ptr::eq` hack

`command_override_for_token` / `selected_command_override`
(`src/cli.rs:356-387`) operate on `config::CommandCapabilities` maps and are
consumed by `config::scope`. Because they return only the scope value,
`scope.rs::selected_command_entry` (`src/config/scope.rs:241-252`) has to
*search the map again with `std::ptr::eq`* to recover the matched key name.

**Fix:** move both functions into `config` (e.g. `config/scope.rs` or a small
`config/commands.rs`) and return `(&str, &ScopeConfig)` directly. `cli.rs`
keeps only `builtin_command` and re-exports what the registry needs. Deletes
the pointer-identity scan and one cross-module dependency in the wrong
direction (cli → config internals).

### ARCH-4: restructure `parse_arguments` as one positional scan

The `get_all` + reversed-span-drain approach is the root cause of BUG-1,
BUG-5, BUG-6, and the `--pretty` eating; correctness depends on extraction
*order* (e.g. `--driver` must be extracted before the bare-word `matrix`
scan). Replace it with a single left-to-right pass:

1. Walk tokens up to `--`, tracking whether the subcommand position has been
   seen (reuse `subcommand_token_index`'s skip rules).
2. A table of cargo-fc options (`name`, `takes_value`, `apply`) drives
   extraction; everything unrecognized is forwarded verbatim.
3. `matrix`/`version` only match at the subcommand position; `--pretty` only
   in matrix mode; `--help`/`-h` only before the subcommand position (UX-6).

This is still a zero-dependency hand parser but drops the multi-pass
subtlety, makes last-wins the natural behavior, and shrinks
`parse_arguments`'s `#[expect(too_many_lines)]`. The existing tests carry
over; add cases for the BUG-1 scenarios.

### ARCH-5: derive validation key lists from the schema

`validate.rs` hand-maintains `FEATURE_MATRIX_KEYS` and `PATCH_TYPED_KEYS`;
adding a field to `FeatureMatrixPatch` without updating them makes valid
config *rejected*. `FLAG_KEYS` already avoids this via the macro.

- Cheap (do first): add a drift test asserting
  `FEATURE_MATRIX_KEYS == keys_for(FeatureMatrixPatch::default())` — the
  `keys_for` serde-based helper already exists in `schema.rs` tests.
- Optional: replace the `valid_in` match with a declarative
  `&[(SettingKind, &[ScopeId])]` table so the scope matrix reads like the
  README's documentation table. The `Err("")` + double-`valid_in`-call flow
  in `validate_scope` (`src/config/validate.rs:114-143`, where `bail_unknown`
  always errors so the `continue` after it is unreachable) gets cleaned up in
  the same pass.

BUG-4's shape validation extends this same table with a `ValueShape` per
kind.

### ARCH-6: delete duplicated and CLI-dead public API

The lib API is explicitly unstable ("the command-line interface is the
supported interface"), so pruning is allowed:

- `implication::maybe_prune` (`src/implication.rs:90-116`) re-derives
  `no_prune_implied`/`prune_implied` resolution — logic that otherwise lives
  only in `ResolvedFlags::from_config` — and is used only by integration
  tests. Migrate `tests/prune_implied.rs` to
  `resolve_config` + a public thin wrapper over the resolved-flag variant,
  then delete it. This removes the last duplicate of the prune-flag
  resolution rule.
- `Workspace::packages_for_fc` is used only in unit tests; the binary uses
  `candidate_packages_for_fc` + per-target exclusion. Drop it from the trait
  (tests can compose the two calls).
- Merge `print_warning!`/`print_note!` into one
  `print_labeled!(label, color, ...)` macro with two thin wrappers
  (`src/lib.rs:57-105`).
- `ResolvedFeatures::from_config` + `apply_single_feature_patch` +
  `apply_single_string_patch` + `apply_single_feature_set_patch`
  (`src/config/resolve.rs:44-49`, `263-323`) are a second, parallel
  patch-application path kept only for the no-target public entry point.
  Make single-patch application infallible in the engine (a one-entry layer
  cannot conflict): add `SetPatchOps::from_single(&P) -> SetPatchOps<Elem>`
  and have `from_config` reuse the engine. Deletes ~60 lines and the risk of
  the two paths diverging on add/remove semantics.

### ARCH-7: split `cli.rs` responsibilities (optional, after ARCH-3/4)

Post ARCH-3/4, `cli.rs` still holds: argument parsing, the builtin-command
registry, the `known_quiet_cargo_subcommand` list (~90 entries), and a 180-line
help string. Split into `cli/mod.rs` (parse), `cli/registry.rs` (builtins +
quiet list), `cli/help.rs` (text). Pure file organization; do it last so the
other diffs stay reviewable.

On the quiet list itself: it is a maintenance treadmill, but the alternatives
(probing `cargo <cmd> --help`, warning always, never warning) are all worse.
Keep it; consider demoting the capability hint from `warning` to `note` so an
unlisted third-party subcommand is less alarming, and stating in the hint that
`expand_targets = false` permanently silences it (it already does).

### ARCH-8: not worth doing (evaluated, rejected)

- Unifying `TargetListPatch` with the `SetPatchInput` engine: the ordered
  semantics genuinely differ (declaration order, ordered dedup); a generic
  "collection strategy" abstraction would cost more than the ~60 duplicated
  lines it saves.
- Adopting `clap`/`lexopt`: the parser must forward unknown args verbatim and
  treat position specially; a hand parser restructured per ARCH-4 is simpler
  than fighting a framework.
- Parallel per-combination execution: cargo's own lock serializes builds in a
  target dir; `--aggregate-targets` is already the right lever.

---

## 4. Smaller notes / nice-to-haves

- `MAX_FEATURE_COMBINATIONS` (`src/package.rs:12`) is hardcoded at 100 000.
  A `max_combinations` config key (package scope, validated like other flag
  keys) is a cheap escape hatch for large crates that deliberately restrict
  via `only_features` math.
- CLI flags are set-true-only; config is tri-state. Supporting
  `--flag=false` (e.g. `--fail-fast=false` overriding workspace config) would
  complete the model. Low priority; note that today `--summary-only=false` is
  silently *forwarded to cargo* and errors — with ARCH-4 it should at least
  produce a clear cargo-fc error.
- `Cargo.toml` uses `license-file = "LICENSE"` for what is a standard MIT
  text; `license = "MIT"` gives crates.io/tooling proper SPDX metadata.
- `docs/GITHUB_ACTIONS.md` + `action.yml`: re-verify the matrix examples
  against the current `matrix` output shape (`features`/`metadata`/`name`/
  `target` rows) after any output changes; not audited in depth here.
- Grammar nit in the too-many-configurations error: "feature(s)" but always
  "combinations" — fine, skip.

## 5. Suggested milestones

- **M1 — parser correctness** (BUG-1, BUG-5, BUG-6, UX-3, UX-5, UX-6,
  parts of UX-10): implement ARCH-4 with the fixes folded in; this is one
  coherent diff in `cli.rs` + `lib.rs` with new tests for each regression.
- **M2 — silent-failure elimination** (BUG-3, BUG-4, UX-7, UX-8, BUG-7,
  ARCH-5 drift test): all are "error/warn instead of silently doing the wrong
  thing"; small independent diffs.
- **M3 — execution semantics** (BUG-2, UX-1, UX-2, UX-4): runner behavior
  changes; UX-2 needs a deliberate decision because it changes observable
  stream behavior — document it in the changelog.
- **M4 — architecture** (ARCH-1, ARCH-2, ARCH-3, ARCH-6): behavior-preserving
  refactors, each landable independently; run the full test suite between
  each.
- **M5 — polish** (UX-9, UX-11, UX-12, section 4, ARCH-7).

Rules of thumb while executing: every BUG/UX fix lands with a test that fails
before the fix; refactors (M4) land with zero test-expectation changes; no
legacy config spelling (`skip_feature_sets`, `denylist`, `exact_combinations`,
metadata key aliases, `dedup`, root-package `exclude_packages`) may stop
working.
