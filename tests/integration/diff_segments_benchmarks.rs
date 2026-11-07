use git_ai::authorship::attribution_tracker::{Attribution, AttributionTracker};
use std::time::{Duration, Instant};

const WARMUP_RUNS: usize = 2;
const TIMED_RUNS: usize = 20;
const HUMAN_AUTHOR: &str = "h_bench";
const AI_AUTHOR: &str = "ai_bench";

#[derive(Clone, Copy)]
enum Edit {
    Single,
    Scattered,
    Block,
    Append,
}

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
}

fn source_line(index: usize, changed: bool) -> String {
    if changed {
        format!(
            "    if check_{index}() {{ return handle_{}({index}); }}\n",
            index % 7
        )
    } else {
        format!(
            "    let value_{index} = compute_{}(input_{index});\n",
            index % 17
        )
    }
}

fn base_file(lines: usize) -> String {
    (0..lines).map(|index| source_line(index, false)).collect()
}

fn edited_file(lines: usize, edit: Edit) -> String {
    let mut rng = Lcg(42);
    let mut out = String::new();
    for index in 0..lines {
        let changed = match edit {
            Edit::Single => index == lines / 2,
            Edit::Scattered => rng.next() % 20 == 0,
            Edit::Block => index >= lines / 4 && index < lines / 4 + lines / 2,
            Edit::Append => false,
        };
        out.push_str(&source_line(index, changed));
    }
    if matches!(edit, Edit::Append) {
        for index in lines..lines + 50 {
            out.push_str(&source_line(index, false));
        }
    }
    out
}

fn time_runs(mut run: impl FnMut()) -> (Duration, Duration) {
    for _ in 0..WARMUP_RUNS {
        run();
    }
    let mut samples: Vec<Duration> = (0..TIMED_RUNS)
        .map(|_| {
            let start = Instant::now();
            run();
            start.elapsed()
        })
        .collect();
    samples.sort();
    (samples[TIMED_RUNS / 2], samples[TIMED_RUNS * 95 / 100])
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

#[test]
fn diff_segments_cover_the_whole_new_file() {
    let tracker = AttributionTracker::new();
    let old = base_file(200);
    for edit in [Edit::Single, Edit::Scattered, Edit::Block, Edit::Append] {
        let new = edited_file(200, edit);

        let segments = tracker.compute_diff_segments(&old, &new).unwrap();

        assert!(segments.iter().any(|segment| segment.changed));
        assert_eq!(segments.last().unwrap().new_range.1, new.len());
        assert_eq!(segments.last().unwrap().old_range.1, old.len());
    }
}

#[test]
fn identical_content_has_no_changed_segment() {
    let tracker = AttributionTracker::new();
    let content = base_file(200);

    let segments = tracker.compute_diff_segments(&content, &content).unwrap();

    assert!(segments.iter().all(|segment| !segment.changed));
}

#[test]
#[ignore]
fn benchmark_diff_segments_against_the_checkpoint_diff() {
    let tracker = AttributionTracker::new();
    println!(
        "{:>7} {:>9} {:<12} | {:>18} | {:>14} | {:>18} | {:>9}",
        "lines",
        "bytes",
        "edit",
        "existing AI p50/95",
        "existing human",
        "new segments p50/95",
        "new / AI"
    );
    for lines in [1_000usize, 10_000, 50_000] {
        let old = base_file(lines);
        let old_attributions = vec![Attribution::new(0, old.len(), HUMAN_AUTHOR.to_string(), 0)];
        for (name, edit) in [
            ("single line", Edit::Single),
            ("scattered 5%", Edit::Scattered),
            ("block 50%", Edit::Block),
            ("append 50", Edit::Append),
        ] {
            let new = edited_file(lines, edit);
            let existing_ai = time_runs(|| {
                tracker
                    .update_attributions_for_checkpoint(
                        &old,
                        &new,
                        &old_attributions,
                        AI_AUTHOR,
                        1,
                        true,
                    )
                    .unwrap();
            });
            let existing_human = time_runs(|| {
                tracker
                    .update_attributions_for_checkpoint(
                        &old,
                        &new,
                        &old_attributions,
                        HUMAN_AUTHOR,
                        1,
                        false,
                    )
                    .unwrap();
            });
            let segments = time_runs(|| {
                tracker.compute_diff_segments(&old, &new).unwrap();
            });
            println!(
                "{:>7} {:>9} {:<12} | {:>8.1}ms /{:>6.1}ms | {:>12.1}ms | {:>8.1}ms /{:>6.1}ms | {:>8.0}%",
                lines,
                new.len(),
                name,
                ms(existing_ai.0),
                ms(existing_ai.1),
                ms(existing_human.0),
                ms(segments.0),
                ms(segments.1),
                ms(segments.0) / ms(existing_ai.0) * 100.0,
            );
        }
    }
}
