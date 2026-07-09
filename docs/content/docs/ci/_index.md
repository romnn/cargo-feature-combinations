---
title: Continuous integration
weight: 8
bookCollapseSection: true
---

# Continuous integration

`cargo fc` fits CI two ways:

1. **One job, the whole matrix.** Run `cargo fc check` (or `clippy`) in a single job and let it iterate every combination — and every [configured target]({{< relref "../targets/configured-targets.md" >}}).
2. **Fan out.** Use `cargo fc matrix` to emit a JSON matrix, then run one parallel job per combination.

- **[GitHub Actions]({{< relref "github-actions.md" >}})** — complete workflows for both approaches.
