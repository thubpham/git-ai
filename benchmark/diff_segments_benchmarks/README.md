# Diff segments benchmark

Measures what the span diff (`AttributionTracker::compute_diff_segments`) costs next to the diff a checkpoint already runs (`update_attributions_for_checkpoint`).

The benchmark is `tests/integration/diff_segments_benchmarks.rs`, registered in `tests/integration/main.rs`. It generates its own source files in code, from a fixed seed, so every run times the same content. This folder holds the method and the recorded results.

## Run

```bash
cargo test --release --test integration benchmark_diff_segments -- --ignored --nocapture
```

The benchmark is `#[ignore]`, so normal test runs and CI skip it. Use `--release`. Debug timings are not representative.

The same file has two quick tests that run normally and check the benchmark's inputs:

```bash
cargo test --test integration diff_segments
```

## What it times

For each file size and edit type, over 20 timed runs after 2 warm-up runs:

- **Existing AI**: `update_attributions_for_checkpoint(..., is_ai = true)`. The coarse whole-line ownership diff an AI checkpoint pays today.
- **Existing human**: the same call with `is_ai = false`. This already runs the token-aligned diff.
- **New span diff**: `compute_diff_segments`. The extra diff span tracking needs.
- **New / AI**: new span diff p50 as a percentage of existing AI p50.

Sizes are 1,000, 10,000 and 50,000 lines. Files above 256 KB use the diff's fast path, so the sizes cover both regimes. Edit types are a single line, scattered 5%, a contiguous 50% block, and a 50-line append.

## Results

Recorded 2026-11-06. Release build, one machine, synthetic code-like lines. Median (p50) times.

| Lines | Bytes | Edit | Existing AI | Existing human | New span diff | New / AI |
|---|---|---|---|---|---|---|
| 1,000 | 42,193 | single line | 0.2 ms | 0.6 ms | 0.2 ms | 102% |
| 1,000 | 42,290 | scattered 5% | 0.3 ms | 0.5 ms | 0.5 ms | 168% |
| 1,000 | 43,482 | block 50% | 0.2 ms | 12.7 ms | 12.2 ms | 4929% |
| 1,000 | 44,410 | append 50 | 0.2 ms | 0.3 ms | 0.3 ms | 123% |
| 10,000 | 441,899 | single line | 2.5 ms | 6.1 ms | 2.5 ms | 100% |
| 10,000 | 443,137 | scattered 5% | 2.8 ms | 6.4 ms | 5.2 ms | 185% |
| 10,000 | 454,838 | block 50% | 2.5 ms | 8.7 ms | 2.5 ms | 100% |
| 10,000 | 444,217 | append 50 | 2.5 ms | 2.5 ms | 2.6 ms | 103% |
| 50,000 | 2,298,369 | single line | 12.8 ms | 38.8 ms | 15.6 ms | 122% |
| 50,000 | 2,304,773 | scattered 5% | 17.0 ms | 53.5 ms | 31.4 ms | 184% |
| 50,000 | 2,363,072 | block 50% | 12.9 ms | 50.9 ms | 12.8 ms | 100% |
| 50,000 | 2,300,688 | append 50 | 13.2 ms | 13.0 ms | 15.2 ms | 115% |

p95 stayed close to p50 in nearly every row. A second run on the same machine matched these medians within noise. The exceptions were outliers at 50,000 lines (p95 up to 90 ms) that moved between runs, so treat p95 at that size as unreliable.

## Reading the results

- Typically the new diff costs about as much as the existing AI diff (100% to 185%). In absolute terms that is 0.2 to 0.5 ms at 1,000 lines, 2.5 to 5 ms at 10,000 lines, and 13 to 31 ms at 50,000 lines.
- One bad case: a heavy rewrite of a mid-size file (1,000 lines, 50% changed) costs 12 ms, about 60 times the AI diff. Larger files take the cheap fast path. Mid-size files take the token-aligned diff, which is expensive on a big rewrite. Human checkpoints already pay this cost today (the Existing human column).
- A human checkpoint's existing diff and the span diff are the same work, so they could share one computation.
- An AI checkpoint skips the token diff on purpose, so the span diff is genuinely needed there.

## Limits

- The files are synthetic and repetitive, not real repositories.
- One machine, 20 samples per case.
- Only the diff functions are timed, not the whole checkpoint pipeline or any clustering work.
