# Optimization records

One file per optimization. An optimization stays **off by default** until its
record contains in-game benchmark results that pass the acceptance rule in
[`../BENCHMARKING.md`](../BENCHMARKING.md).

| ID | Name | Layer | Status | Default |
|---|---|---|---|---|
| [0001](0001-redundant-state-filter.md) | Redundant D3D9 state filter | D3D9 | implemented, correctness-tested, **not benchmarked in-game** | off |

## Template

```markdown
# NNNN — <name>

Status: <proposed | implemented | benchmarked | default-on | rejected>

## What it changes
## Why it should improve performance  (+ which measurement supports it)
## Functions intercepted  (method / address, signature, calling convention, verification)
## Correctness argument and tests
## Compatibility risks
## Benchmark results  (configuration, scenes, sessions, numbers, verdict)
## How to disable
```
