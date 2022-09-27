## serde_json_merge

[<img alt="build status" src="https://img.shields.io/github/workflow/status/romnn/serde_json_merge/build?label=build">](https://github.com/romnn/serde_json_merge/actions/workflows/build.yml)
[<img alt="test status" src="https://img.shields.io/github/workflow/status/romnn/serde_json_merge/test?label=test">](https://github.com/romnn/serde_json_merge/actions/workflows/test.yml)
[<img alt="benchmarks" src="https://img.shields.io/github/workflow/status/romnn/serde_json_merge/bench?label=bench">](https://romnn.github.io/serde_json_merge/)
[<img alt="crates.io" src="https://img.shields.io/crates/v/serde_json_merge">](https://crates.io/crates/serde_json_merge)
[<img alt="docs.rs" src="https://img.shields.io/docsrs/serde_json_merge/latest?label=docs.rs">](https://docs.rs/serde_json_merge)

Merge, index, iterate, and sort a ``serde_json::Value`` (recursively).

This library supports in-place merging and sorting using DFS and BFS traversal unline most implementations out there that use recursion and can stack overflow.

```toml
[dependencies]
serde_json_merge = "0"
```

#### Usage

For usage examples, check the [examples](https://github.com/romnn/serde_json_merge/tree/main/examples) and [documentation](https://docs.rs/serde_json_merge).

#### Examples

```bash
cargo run --example async_fs --features async -- --path ./
cargo run --example sync_fs --features sync,rayon -- --path ./
```

#### Documentation

```bash
RUSTDOCFLAGS="--cfg docsrs" cargo +nightly doc --all-features
```

#### Linting

```bash
cargo clippy --tests --benches --examples -- -Dclippy::all -Dclippy::pedantic
```

#### Benchmarking

```bash
cargo install cargo-criterion
# full benchmark suite
cargo criterion --features full
# sync benchmarks only
cargo criterion --features sync -- sync
# dfs benchmarks only
cargo criterion --features full -- dfs
```

Benchmark reports from CI are published are available [here](https://romnn.github.io/serde_json_merge/).

#### Acknowledgements

After i wrote this crate for another project and decided to publish it, I found [json_value_merge](https://crates.io/crates/json_value_merge).

Looking through it, I added `merge_index` inspired by their `merge_in` API.

#### TODO
- write benchmarks
- add globbing iter

- add iters for keys and values
- implement sorting values with indices
- implement bfs
- add rayon support using par-dfs
- write documentation
- add examples in the documentation

DONE:
- inline everything
- do we really need the any type? so useless right now :(
  - maybe use them for the very precise type?
- add custom comparator for merging
- split the sorting into extra module
- implement unstable sorting
- add feature gates for sort and merge
- add few more tests for kind and so on
- partial eq can be written top level
- add limit to dfs
- do not expose wrapper for Value but use extension
- add depth parameter to recursive merge
