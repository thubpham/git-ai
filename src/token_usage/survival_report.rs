use serde::{Deserialize, Serialize};

use std::collections::HashSet;

use super::cost::price_entry;
use super::facts_db::{CostCall, FactsDatabase, FileSurvival, LossShare, SliceDimension};
use super::survival::{Horizon, SurvivalAnchor};
use super::survival_stats::{
    DAY_SECS, Lifetime, kaplan_meier, lifetime, rmst, session_bootstrap, survival_at,
};
use super::types::{PricingShape, Speed, TokenCounts, UsageEntry};
use crate::error::GitAiError;

pub const EXAMPLE_MIN_BYTES: i64 = 1024;
pub const EXAMPLE_LIMIT: usize = 3;
pub const DEATH_THRESHOLD: f64 = 0.5;
const SENSITIVITY_THRESHOLDS: [f64; 4] = [0.25, 0.5, 0.75, 1.0];
const CURVE_DAYS: [u32; 5] = [1, 3, 7, 14, 30];
const RMST_DAYS: [u32; 2] = [14, 30];
const BOOTSTRAP_RESAMPLES: usize = 1000;
const BOOTSTRAP_SEED: u64 = 8;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SurvivalReport {
    pub demo: bool,
    pub totals: Totals,
    pub commit_rate: CommitRate,
    pub horizons: Vec<HorizonRow>,
    pub loss: LossReport,
    pub slices: Vec<SliceGroup>,
    pub examples: Vec<ExampleGroup>,
    pub cost: Option<Vec<CostRow>>,
    pub stats: Option<SurvivalStats>,
    pub weightings: Vec<WeightingRow>,
    pub quality: Quality,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Totals {
    pub spans: i64,
    pub bytes_written: i64,
    pub bytes_alive: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommitRate {
    pub spans: i64,
    pub bytes_written: i64,
    pub bytes_committed: i64,
    pub rate: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HorizonRow {
    pub anchor: SurvivalAnchor,
    pub horizon: Horizon,
    pub spans: i64,
    pub surviving_bytes: i64,
    pub total_bytes: i64,
    pub retention: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LossRow {
    pub kind: String,
    pub bytes: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LossReport {
    pub by_who: Vec<LossRow>,
    pub by_how: Vec<LossRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SliceRowOut {
    pub label: String,
    pub spans: i64,
    pub surviving_bytes: i64,
    pub total_bytes: i64,
    pub retention: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SliceGroup {
    pub dimension: SliceDimension,
    pub anchor: SurvivalAnchor,
    pub horizon: Horizon,
    pub rows: Vec<SliceRowOut>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileRow {
    pub repo: String,
    pub file: String,
    pub surviving_bytes: i64,
    pub total_bytes: i64,
    pub retention: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExampleGroup {
    pub anchor: SurvivalAnchor,
    pub horizon: Horizon,
    pub best: Vec<FileRow>,
    pub worst: Vec<FileRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostRow {
    pub model: String,
    pub horizon: Horizon,
    pub direct_usd: f64,
    pub full_usd: f64,
    pub surviving_bytes: i64,
    pub direct_per_kb: f64,
    pub full_per_kb: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Quality {
    pub spans: i64,
    pub pinned: i64,
    pub linked: i64,
    pub pending: i64,
    pub unknown: i64,
    pub none: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SurvivalStats {
    pub threshold: f64,
    pub spans: i64,
    pub deaths: i64,
    pub followed_days: f64,
    pub curve: Vec<CurveRow>,
    pub rmst: Vec<RmstRow>,
    pub sensitivity: Vec<SensitivityRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CurveRow {
    pub day: u32,
    pub survival: f64,
    pub low: f64,
    pub high: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RmstRow {
    pub limit_days: u32,
    pub days: f64,
    pub low: f64,
    pub high: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SensitivityRow {
    pub threshold: f64,
    pub deaths: i64,
    pub survival_day_7: Option<f64>,
    pub rmst_14: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WeightingRow {
    pub horizon: Horizon,
    pub spans: i64,
    pub sessions: i64,
    pub per_byte: f64,
    pub per_span: f64,
    pub per_session: f64,
}

impl SurvivalReport {
    pub fn empty() -> Self {
        SurvivalReport {
            demo: false,
            totals: Totals {
                spans: 0,
                bytes_written: 0,
                bytes_alive: 0,
            },
            commit_rate: CommitRate {
                spans: 0,
                bytes_written: 0,
                bytes_committed: 0,
                rate: 0.0,
            },
            horizons: Vec::new(),
            loss: LossReport {
                by_who: Vec::new(),
                by_how: Vec::new(),
            },
            slices: Vec::new(),
            examples: Vec::new(),
            cost: None,
            stats: None,
            weightings: Vec::new(),
            quality: Quality {
                spans: 0,
                pinned: 0,
                linked: 0,
                pending: 0,
                unknown: 0,
                none: 0,
            },
        }
    }

    pub fn build(db: &FactsDatabase, now: i64) -> Result<SurvivalReport, GitAiError> {
        let totals = db.span_totals()?;
        let commit_rate = db.commit_rate()?;
        let loss = db.loss()?;
        let quality = db.span_quality()?;
        let mut slices: Vec<SliceGroup> = Vec::new();
        for dimension in SliceDimension::ALL {
            for row in db.slice(dimension)? {
                let out = SliceRowOut {
                    retention: ratio(row.surviving_bytes, row.total_bytes),
                    label: row.label,
                    spans: row.spans,
                    surviving_bytes: row.surviving_bytes,
                    total_bytes: row.total_bytes,
                };
                match slices.last_mut() {
                    Some(group)
                        if group.dimension == dimension
                            && group.anchor == row.anchor
                            && group.horizon == row.horizon =>
                    {
                        group.rows.push(out)
                    }
                    _ => slices.push(SliceGroup {
                        dimension,
                        anchor: row.anchor,
                        horizon: row.horizon,
                        rows: vec![out],
                    }),
                }
            }
        }
        let mut examples = Vec::new();
        for anchor in SurvivalAnchor::ALL {
            for horizon in Horizon::ALL {
                let found = db.file_examples(anchor, horizon, EXAMPLE_MIN_BYTES, EXAMPLE_LIMIT)?;
                if found.best.is_empty() && found.worst.is_empty() {
                    continue;
                }
                examples.push(ExampleGroup {
                    anchor,
                    horizon,
                    best: found.best.into_iter().map(file_row).collect(),
                    worst: found.worst.into_iter().map(file_row).collect(),
                });
            }
        }
        Ok(SurvivalReport {
            demo: false,
            totals: Totals {
                spans: totals.spans,
                bytes_written: totals.bytes_written,
                bytes_alive: totals.bytes_alive,
            },
            commit_rate: CommitRate {
                rate: ratio(commit_rate.bytes_committed, commit_rate.bytes_written),
                spans: commit_rate.spans,
                bytes_written: commit_rate.bytes_written,
                bytes_committed: commit_rate.bytes_committed,
            },
            horizons: db
                .survival_report()?
                .into_iter()
                .map(|row| HorizonRow {
                    anchor: row.anchor,
                    horizon: row.horizon,
                    spans: row.spans,
                    surviving_bytes: row.surviving_bytes,
                    total_bytes: row.total_bytes,
                    retention: row.retention(),
                })
                .collect(),
            loss: LossReport {
                by_who: loss.by_who.into_iter().map(loss_row).collect(),
                by_how: loss.by_how.into_iter().map(loss_row).collect(),
            },
            slices,
            examples,
            cost: cost_rows(db)?,
            stats: survival_stats(db, now)?,
            weightings: db
                .retention_weightings()?
                .into_iter()
                .map(|row| WeightingRow {
                    horizon: row.horizon,
                    spans: row.spans,
                    sessions: row.sessions,
                    per_byte: row.per_byte,
                    per_span: row.per_span,
                    per_session: row.per_session,
                })
                .collect(),
            quality: Quality {
                spans: quality.spans,
                pinned: quality.pinned,
                linked: quality.linked,
                pending: quality.pending,
                unknown: quality.unknown,
                none: quality.none,
            },
        })
    }
}

fn ratio(surviving: i64, total: i64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    surviving as f64 / total as f64
}

fn survival_stats(db: &FactsDatabase, now: i64) -> Result<Option<SurvivalStats>, GitAiError> {
    let histories = db.committed_span_histories()?;
    if histories.is_empty() {
        return Ok(None);
    }
    let lifetimes_at = |threshold: f64| -> Vec<Lifetime> {
        histories
            .iter()
            .map(|history| lifetime(history, threshold, now))
            .collect()
    };
    let lifetimes = lifetimes_at(DEATH_THRESHOLD);
    let followed_days = histories
        .iter()
        .map(|history| (now - history.pinned_at).max(0) as f64 / DAY_SECS)
        .fold(0.0, f64::max);
    let curve = kaplan_meier(&lifetimes);
    let reached = |day: u32| f64::from(day) <= followed_days;
    let range = |statistic: &dyn Fn(&[Lifetime]) -> f64| {
        session_bootstrap(&lifetimes, BOOTSTRAP_RESAMPLES, BOOTSTRAP_SEED, statistic)
    };
    let curve_rows = CURVE_DAYS
        .into_iter()
        .filter(|&day| reached(day))
        .map(|day| {
            let at = f64::from(day);
            let (low, high) = range(&|sample| survival_at(&kaplan_meier(sample), at));
            CurveRow {
                day,
                survival: survival_at(&curve, at),
                low,
                high,
            }
        })
        .collect();
    let rmst_rows = RMST_DAYS
        .into_iter()
        .filter(|&day| reached(day))
        .map(|limit_days| {
            let limit = f64::from(limit_days);
            let (low, high) = range(&|sample| rmst(&kaplan_meier(sample), limit));
            RmstRow {
                limit_days,
                days: rmst(&curve, limit),
                low,
                high,
            }
        })
        .collect();
    let sensitivity = SENSITIVITY_THRESHOLDS
        .into_iter()
        .map(|threshold| {
            let at_threshold = lifetimes_at(threshold);
            let curve = kaplan_meier(&at_threshold);
            SensitivityRow {
                threshold,
                deaths: at_threshold.iter().filter(|lifetime| lifetime.died).count() as i64,
                survival_day_7: reached(7).then(|| survival_at(&curve, 7.0)),
                rmst_14: reached(14).then(|| rmst(&curve, 14.0)),
            }
        })
        .collect();
    Ok(Some(SurvivalStats {
        threshold: DEATH_THRESHOLD,
        spans: lifetimes.len() as i64,
        deaths: lifetimes.iter().filter(|lifetime| lifetime.died).count() as i64,
        followed_days,
        curve: curve_rows,
        rmst: rmst_rows,
        sensitivity,
    }))
}

fn cost_rows(db: &FactsDatabase) -> Result<Option<Vec<CostRow>>, GitAiError> {
    let retained = db.retained_bytes_by_model()?;
    if retained.is_empty() {
        return Ok(None);
    }
    let priced: Vec<(CostCall, f64)> = db
        .cost_calls()?
        .into_iter()
        .map(|call| {
            let usd = call_usd(&call);
            (call, usd)
        })
        .collect();
    let owners = db.measured_span_owners()?;
    let rows = retained
        .into_iter()
        .map(|retained| {
            let at_age = owners
                .iter()
                .filter(|owner| owner.horizon == retained.horizon);
            let prompts: HashSet<&str> = at_age
                .clone()
                .filter_map(|owner| owner.prompt_id.as_deref())
                .collect();
            let sessions: HashSet<&str> = at_age.map(|owner| owner.session_id.as_str()).collect();
            let of_model = priced
                .iter()
                .filter(|(call, _)| call.model == retained.model);
            let full_usd: f64 = of_model
                .clone()
                .filter(|(call, _)| {
                    call.session_id
                        .as_deref()
                        .is_some_and(|session| sessions.contains(session))
                })
                .fold(0.0, |total, (_, usd)| total + usd);
            let direct_usd: f64 = of_model
                .filter(|(call, _)| {
                    call.prompt_id
                        .as_deref()
                        .is_some_and(|prompt| prompts.contains(prompt))
                })
                .fold(0.0, |total, (_, usd)| total + usd);
            CostRow {
                direct_per_kb: per_kb(direct_usd, retained.surviving_bytes),
                full_per_kb: per_kb(full_usd, retained.surviving_bytes),
                model: retained.model,
                horizon: retained.horizon,
                direct_usd,
                full_usd,
                surviving_bytes: retained.surviving_bytes,
            }
        })
        .collect();
    Ok(Some(rows))
}

fn call_usd(call: &CostCall) -> f64 {
    let speed: Option<Speed> = call
        .speed
        .as_deref()
        .and_then(|speed| serde_json::from_value(serde_json::Value::from(speed)).ok());
    let entry = UsageEntry {
        entry_key: String::new(),
        message_id: None,
        ts: 0,
        model: call.priced_model.clone(),
        tokens: TokenCounts {
            input: call.input_tokens,
            output: call.output_tokens,
            cache_read: call.cache_read_tokens,
            cache_write: call.cache_write_tokens,
            reasoning_output: None,
            total: call
                .input_tokens
                .saturating_add(call.output_tokens)
                .saturating_add(call.cache_read_tokens)
                .saturating_add(call.cache_write_tokens),
        },
        cache_write_1h: call.cache_write_1h_tokens,
        transcript_cost_micro_usd: None,
        is_sidechain: false,
        speed,
        speed_inferred: speed.is_none(),
        pricing_shape: PricingShape::Claude,
    };
    price_entry(&entry)
        .cost_micro_usd
        .map_or(0.0, |micro_usd| micro_usd as f64 / 1_000_000.0)
}

fn per_kb(usd: f64, bytes: i64) -> f64 {
    if bytes == 0 {
        return 0.0;
    }
    usd / (bytes as f64 / 1024.0)
}

fn loss_row(share: LossShare) -> LossRow {
    LossRow {
        kind: share.kind,
        bytes: share.bytes,
    }
}

fn file_row(file: FileSurvival) -> FileRow {
    FileRow {
        retention: ratio(file.surviving_bytes, file.total_bytes),
        repo: file.repo_key,
        file: file.file_path,
        surviving_bytes: file.surviving_bytes,
        total_bytes: file.total_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token_usage::spans::{EditType, PriorKind};

    const DAY: i64 = 24 * 60 * 60;

    fn db() -> (tempfile::TempDir, FactsDatabase) {
        let dir = tempfile::tempdir().unwrap();
        let db = FactsDatabase::open(dir.path().join("transcript-facts-db")).unwrap();
        (dir, db)
    }

    fn big_span(span_id: &str, file_path: &str) -> crate::token_usage::facts_db::NewSpan {
        crate::token_usage::facts_db::NewSpan {
            span_id: span_id.to_string(),
            session_id: "s1".to_string(),
            tool_use_id: Some(format!("toolu_{span_id}")),
            prompt_id: None,
            repo_key: "/repo/.git".to_string(),
            file_path: file_path.to_string(),
            author_kind: "ai".to_string(),
            author_id: "author-1".to_string(),
            source: "file_edit".to_string(),
            content_hash: "hash-1".to_string(),
            created_at: 1000,
            prior_kind: PriorKind::Ai,
            edit_type: EditType::Replace,
            pieces: vec![(0, 2000)],
        }
    }

    fn measured_db() -> (tempfile::TempDir, FactsDatabase) {
        let (dir, db) = db();
        for (span_id, file_path) in [("s1", "src/a.rs"), ("s2", "src/b.rs")] {
            db.insert_span(&big_span(span_id, file_path)).unwrap();
            db.pin_spans(
                "/repo/.git",
                "commit-1",
                3000,
                &std::collections::HashMap::from([(file_path.to_string(), "blob-1".to_string())]),
            )
            .unwrap();
        }
        db.measure_due_spans(3000 + 2 * DAY).unwrap();
        (dir, db)
    }

    #[test]
    fn building_from_an_empty_database_gives_an_empty_report() {
        let (_dir, db) = db();

        let report = SurvivalReport::build(&db, 3000 + 2 * DAY).unwrap();

        assert_eq!(report, SurvivalReport::empty());
        assert!(!report.demo);
        assert_eq!(report.cost, None);
    }

    #[test]
    fn building_collects_every_section_from_measured_spans() {
        let (_dir, db) = measured_db();

        let report = SurvivalReport::build(&db, 3000 + 2 * DAY).unwrap();

        assert!(!report.demo);
        assert_eq!(report.totals.spans, 2);
        assert_eq!(report.totals.bytes_written, 4000);
        assert_eq!(
            report.commit_rate,
            CommitRate {
                spans: 2,
                bytes_written: 4000,
                bytes_committed: 4000,
                rate: 1.0,
            }
        );
        assert_eq!(report.horizons.len(), 2);
        assert_eq!(report.horizons[0].anchor, SurvivalAnchor::Commit);
        assert_eq!(report.horizons[0].horizon, Horizon::Day);
        assert_eq!(report.horizons[0].spans, 2);
        assert_eq!(report.horizons[0].surviving_bytes, 4000);
        assert!((report.horizons[0].retention - 1.0).abs() < 1e-9);
        let relationship = report
            .slices
            .iter()
            .find(|group| {
                group.dimension == SliceDimension::Relationship
                    && group.anchor == SurvivalAnchor::Commit
                    && group.horizon == Horizon::Day
            })
            .unwrap();
        assert_eq!(relationship.rows[0].label, "ai");
        assert_eq!(relationship.rows[0].spans, 2);
        let examples = report
            .examples
            .iter()
            .find(|group| group.anchor == SurvivalAnchor::Commit && group.horizon == Horizon::Day)
            .unwrap();
        assert_eq!(examples.best.len(), 2);
        assert_eq!(examples.best[0].file, "src/a.rs");
        assert_eq!(report.quality.pinned, 2);
        assert_eq!(report.cost, None);
    }

    #[test]
    fn cost_rows_price_the_editing_prompt_as_direct_and_the_whole_session_as_full() {
        let (_dir, db) = db();
        let call = |request_id: &str, prompt_id: &str, tool_use_ids: Vec<String>| {
            crate::token_usage::claude_facts::TranscriptFact::Call(Box::new(
                crate::token_usage::claude_facts::CallFact {
                    request_id: request_id.to_string(),
                    session_id: Some("s1".to_string()),
                    prompt_id: Some(prompt_id.to_string()),
                    model: Some("claude-sonnet-4-20250514".to_string()),
                    input_tokens: Some(1000),
                    output_tokens: Some(500),
                    cache_read_tokens: Some(20000),
                    cache_write_tokens: Some(300),
                    tool_use_ids,
                    ..Default::default()
                },
            ))
        };
        db.commit_facts(&[
            call("req-plan", "p0", Vec::new()),
            call("req-edit", "p1", vec!["toolu_s1".to_string()]),
        ])
        .unwrap();
        db.insert_span(&crate::token_usage::facts_db::NewSpan {
            prompt_id: Some("p1".to_string()),
            ..big_span("s1", "src/a.rs")
        })
        .unwrap();
        db.pin_spans(
            "/repo/.git",
            "commit-1",
            3000,
            &std::collections::HashMap::from([("src/a.rs".to_string(), "blob-1".to_string())]),
        )
        .unwrap();
        db.measure_due_spans(3000 + 2 * DAY).unwrap();

        let report = SurvivalReport::build(&db, 3000 + 2 * DAY).unwrap();

        let rows = report.cost.expect("cost rows");
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.model, "claude-sonnet-4-20250514");
        assert_eq!(row.horizon, Horizon::Day);
        assert_eq!(row.surviving_bytes, 2000);
        assert!(row.direct_usd > 0.0);
        assert!((row.full_usd - 2.0 * row.direct_usd).abs() < 1e-9);
        let kb = 2000.0 / 1024.0;
        assert!((row.direct_per_kb - row.direct_usd / kb).abs() < 1e-9);
        assert!((row.full_per_kb - row.full_usd / kb).abs() < 1e-9);
    }

    #[test]
    fn groups_with_no_rows_are_left_out() {
        let (_dir, db) = measured_db();

        let report = SurvivalReport::build(&db, 3000 + 2 * DAY).unwrap();

        assert!(report.slices.iter().all(|group| !group.rows.is_empty()));
        assert!(
            report
                .slices
                .iter()
                .all(|group| group.horizon == Horizon::Day)
        );
        assert!(
            report
                .examples
                .iter()
                .all(|group| !group.best.is_empty() || !group.worst.is_empty())
        );
    }

    #[test]
    fn a_built_report_survives_a_json_round_trip() {
        let (_dir, db) = measured_db();
        let report = SurvivalReport::build(&db, 3000 + 2 * DAY).unwrap();

        let json = serde_json::to_string(&report).unwrap();
        let parsed: SurvivalReport = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed, report);
    }

    #[test]
    fn json_uses_the_names_people_read() {
        let json = serde_json::to_string(&SurvivalReport::demo()).unwrap();

        assert!(json.contains("\"anchor\":\"commit\""), "{json}");
        assert!(json.contains("\"anchor\":\"generation\""), "{json}");
        assert!(json.contains("\"horizon\":\"24h\""), "{json}");
        assert!(json.contains("\"horizon\":\"7d\""), "{json}");
        assert!(json.contains("\"horizon\":\"30d\""), "{json}");
        assert!(json.contains("\"dimension\":\"relationship\""), "{json}");
        assert!(json.contains("\"dimension\":\"model_effort\""), "{json}");
        assert!(json.contains("\"demo\":true"), "{json}");
    }

    #[test]
    fn the_demo_report_is_marked_and_fills_every_section() {
        let demo = SurvivalReport::demo();

        assert!(demo.demo);
        assert!(demo.totals.bytes_written > demo.totals.bytes_alive);
        assert!(demo.totals.bytes_alive > 0);
        assert_eq!(demo.horizons.len(), 6);
        assert!(!demo.loss.by_who.is_empty());
        assert!(!demo.loss.by_how.is_empty());
        for dimension in [
            SliceDimension::Relationship,
            SliceDimension::EditType,
            SliceDimension::Tool,
            SliceDimension::ModelEffort,
        ] {
            assert!(
                demo.slices.iter().any(|group| group.dimension == dimension),
                "{dimension:?}"
            );
        }
        assert!(demo.examples.iter().all(|group| !group.best.is_empty()));
        assert!(demo.examples.iter().all(|group| !group.worst.is_empty()));
        assert_eq!(demo.cost.as_ref().map(Vec::len), Some(9));
        assert!(demo.quality.spans >= demo.quality.pinned);
    }

    #[test]
    fn the_demo_report_is_the_same_every_time() {
        assert_eq!(SurvivalReport::demo(), SurvivalReport::demo());
    }

    #[test]
    fn every_demo_horizon_row_is_internally_consistent() {
        for row in SurvivalReport::demo().horizons {
            assert!(row.surviving_bytes <= row.total_bytes);
            let expected = row.surviving_bytes as f64 / row.total_bytes as f64;
            assert!((row.retention - expected).abs() < 1e-9);
        }
    }

    #[test]
    fn building_adds_the_survival_curve_and_weightings_for_committed_spans() {
        let (_dir, db) = measured_db();

        let report = SurvivalReport::build(&db, 3000 + 2 * DAY).unwrap();

        let stats = report.stats.expect("stats");
        assert_eq!((stats.spans, stats.deaths), (2, 0));
        assert!((stats.threshold - 0.5).abs() < 1e-12);
        assert!((stats.followed_days - 2.0).abs() < 1e-12);
        assert_eq!(
            stats.curve,
            vec![CurveRow {
                day: 1,
                survival: 1.0,
                low: 1.0,
                high: 1.0,
            }]
        );
        assert_eq!(stats.rmst, Vec::<RmstRow>::new());
        assert_eq!(
            stats
                .sensitivity
                .iter()
                .map(|row| row.threshold)
                .collect::<Vec<_>>(),
            vec![0.25, 0.5, 0.75, 1.0]
        );
        assert_eq!(report.weightings.len(), 1);
        assert!((report.weightings[0].per_span - 1.0).abs() < 1e-12);
        assert!((report.weightings[0].per_session - 1.0).abs() < 1e-12);
    }

    #[test]
    fn an_empty_database_has_no_survival_statistics() {
        let (_dir, db) = db();

        let report = SurvivalReport::build(&db, 3000 + 2 * DAY).unwrap();

        assert_eq!(report.stats, None);
        assert!(report.weightings.is_empty());
    }
}
