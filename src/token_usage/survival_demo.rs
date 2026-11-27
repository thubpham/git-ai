use super::facts_db::SliceDimension;
use super::survival::{Horizon, SurvivalAnchor};
use super::survival_report::{
    CommitRate, CostRow, CurveDimension, CurveRow, CurveSlice, DEATH_THRESHOLD, ExampleGroup,
    FileRow, HorizonRow, LossReport, LossRow, Quality, RmstRow, SensitivityRow, SliceGroup,
    SliceRowOut, SurvivalReport, SurvivalStats, Totals, WeightingRow,
};

const AVERAGE_SPAN_BYTES: i64 = 6000;

struct AgeShape {
    anchor: SurvivalAnchor,
    horizon: Horizon,
    spans: i64,
    total_bytes: i64,
    retention: f64,
}

const AGES: [AgeShape; 6] = [
    AgeShape {
        anchor: SurvivalAnchor::Commit,
        horizon: Horizon::Day,
        spans: 1284,
        total_bytes: 2_516_582,
        retention: 0.82,
    },
    AgeShape {
        anchor: SurvivalAnchor::Commit,
        horizon: Horizon::Week,
        spans: 902,
        total_bytes: 1_900_000,
        retention: 0.61,
    },
    AgeShape {
        anchor: SurvivalAnchor::Commit,
        horizon: Horizon::Month,
        spans: 417,
        total_bytes: 900_000,
        retention: 0.44,
    },
    AgeShape {
        anchor: SurvivalAnchor::Generation,
        horizon: Horizon::Day,
        spans: 1310,
        total_bytes: 2_540_000,
        retention: 0.74,
    },
    AgeShape {
        anchor: SurvivalAnchor::Generation,
        horizon: Horizon::Week,
        spans: 930,
        total_bytes: 1_950_000,
        retention: 0.50,
    },
    AgeShape {
        anchor: SurvivalAnchor::Generation,
        horizon: Horizon::Month,
        spans: 430,
        total_bytes: 920_000,
        retention: 0.31,
    },
];

type SliceBase = (&'static str, i64, f64);

const SLICE_BASES: [(SliceDimension, &[SliceBase]); 4] = [
    (
        SliceDimension::Relationship,
        &[
            ("known_human", 212, 0.52),
            ("ai", 148, 0.38),
            ("none", 57, 0.71),
        ],
    ),
    (
        SliceDimension::EditType,
        &[("add", 301, 0.66), ("replace", 116, 0.35)],
    ),
    (
        SliceDimension::Tool,
        &[("file_edit", 350, 0.63), ("bash", 67, 0.22)],
    ),
    (
        SliceDimension::ModelEffort,
        &[
            ("model-a · high", 96, 0.58),
            ("model-a · low", 88, 0.41),
            ("model-b · high", 44, 0.49),
        ],
    ),
];

const MONTH_RETENTION: [(SurvivalAnchor, f64); 2] = [
    (SurvivalAnchor::Commit, 0.44),
    (SurvivalAnchor::Generation, 0.31),
];

fn surviving_of(total_bytes: i64, retention: f64) -> i64 {
    (total_bytes as f64 * retention).round() as i64
}

fn horizon_row(age: &AgeShape) -> HorizonRow {
    let surviving_bytes = surviving_of(age.total_bytes, age.retention);
    HorizonRow {
        anchor: age.anchor,
        horizon: age.horizon,
        spans: age.spans,
        surviving_bytes,
        total_bytes: age.total_bytes,
        retention: surviving_bytes as f64 / age.total_bytes as f64,
    }
}

fn slice_groups() -> Vec<SliceGroup> {
    let mut groups = Vec::new();
    for (dimension, rows) in SLICE_BASES {
        for age in &AGES {
            let month = MONTH_RETENTION
                .iter()
                .find(|(anchor, _)| *anchor == age.anchor)
                .map(|(_, retention)| *retention)
                .unwrap_or(0.0);
            let shift = age.retention - month;
            let month_spans = 417.0;
            groups.push(SliceGroup {
                dimension,
                anchor: age.anchor,
                horizon: age.horizon,
                rows: rows
                    .iter()
                    .map(|(label, spans, retention)| {
                        let spans =
                            ((*spans as f64) * age.spans as f64 / month_spans).round() as i64;
                        let total_bytes = spans * AVERAGE_SPAN_BYTES;
                        let surviving_bytes =
                            surviving_of(total_bytes, (retention + shift).clamp(0.02, 0.98));
                        SliceRowOut {
                            label: label.to_string(),
                            spans,
                            surviving_bytes,
                            total_bytes,
                            retention: surviving_bytes as f64 / total_bytes as f64,
                        }
                    })
                    .collect(),
            });
        }
    }
    groups
}

fn file(path: &str, total_bytes: i64, retention: f64) -> FileRow {
    let surviving_bytes = surviving_of(total_bytes, retention);
    FileRow {
        repo: "demo/.git".to_string(),
        file: path.to_string(),
        surviving_bytes,
        total_bytes,
        retention: surviving_bytes as f64 / total_bytes as f64,
    }
}

fn example_groups() -> Vec<ExampleGroup> {
    AGES.iter()
        .map(|age| ExampleGroup {
            anchor: age.anchor,
            horizon: age.horizon,
            best: vec![
                file("src/token_usage/facts_db.rs", 18_637, 0.96),
                file("src/commands/survival.rs", 9_318, 0.91),
                file("tests/integration/token_usage.rs", 4_506, 0.88),
            ],
            worst: vec![
                file("src/commands/mod.rs", 2_048, 0.04),
                file("scripts/migrate.sh", 1_331, 0.07),
                file("src/commands/git_ai_handlers.rs", 6_963, 0.12),
            ],
        })
        .collect()
}

fn survival_stats() -> SurvivalStats {
    let curve = |day: u32, survival: f64, low: f64, high: f64| CurveRow {
        day,
        survival,
        low,
        high,
    };
    let sensitivity = |threshold: f64, deaths: i64, day_7: f64, rmst_14: f64| SensitivityRow {
        threshold,
        deaths,
        survival_day_7: Some(day_7),
        rmst_14: Some(rmst_14),
    };
    SurvivalStats {
        threshold: DEATH_THRESHOLD,
        spans: 1102,
        deaths: 318,
        followed_days: 41.0,
        curve: vec![
            curve(1, 0.93, 0.90, 0.95),
            curve(3, 0.86, 0.82, 0.89),
            curve(7, 0.79, 0.74, 0.83),
            curve(14, 0.72, 0.66, 0.77),
            curve(30, 0.64, 0.57, 0.70),
        ],
        rmst: vec![
            RmstRow {
                limit_days: 14,
                days: 11.4,
                low: 10.8,
                high: 11.9,
            },
            RmstRow {
                limit_days: 30,
                days: 21.6,
                low: 20.1,
                high: 23.0,
            },
        ],
        sensitivity: vec![
            sensitivity(0.25, 451, 0.70, 10.6),
            sensitivity(0.5, 318, 0.79, 11.4),
            sensitivity(0.75, 246, 0.84, 11.9),
            sensitivity(1.0, 197, 0.88, 12.3),
        ],
    }
}

fn curve_slices() -> Vec<CurveSlice> {
    let slice = |dimension: CurveDimension,
                 label: &str,
                 spans: i64,
                 deaths: i64,
                 followed_days: f64,
                 points: &[(u32, f64)]| CurveSlice {
        dimension,
        label: label.to_string(),
        spans,
        deaths,
        followed_days,
        curve: points
            .iter()
            .map(|&(day, survival)| CurveRow {
                day,
                survival,
                low: (survival - 0.06).max(0.0),
                high: (survival + 0.05).min(1.0),
            })
            .collect(),
    };
    vec![
        slice(
            CurveDimension::Model,
            "model-a",
            684,
            214,
            41.0,
            &[(1, 0.92), (3, 0.84), (7, 0.76), (14, 0.69), (30, 0.61)],
        ),
        slice(
            CurveDimension::Model,
            "model-b",
            418,
            104,
            41.0,
            &[(1, 0.95), (3, 0.89), (7, 0.83), (14, 0.77), (30, 0.69)],
        ),
        slice(
            CurveDimension::CommitWeek,
            "2026-10-19",
            356,
            131,
            41.0,
            &[(1, 0.90), (3, 0.82), (7, 0.73), (14, 0.66), (30, 0.58)],
        ),
        slice(
            CurveDimension::CommitWeek,
            "2026-10-26",
            741,
            186,
            34.0,
            &[(1, 0.94), (3, 0.88), (7, 0.82), (14, 0.75), (30, 0.67)],
        ),
        slice(
            CurveDimension::CommitWeek,
            "2026-11-23",
            5,
            1,
            3.0,
            &[(1, 0.80), (3, 0.80)],
        ),
    ]
}

fn weighting_rows() -> Vec<WeightingRow> {
    vec![
        WeightingRow {
            horizon: Horizon::Day,
            spans: 1046,
            sessions: 212,
            per_byte: 0.91,
            per_span: 0.86,
            per_session: 0.88,
        },
        WeightingRow {
            horizon: Horizon::Week,
            spans: 812,
            sessions: 171,
            per_byte: 0.78,
            per_span: 0.69,
            per_session: 0.74,
        },
        WeightingRow {
            horizon: Horizon::Month,
            spans: 529,
            sessions: 108,
            per_byte: 0.66,
            per_span: 0.55,
            per_session: 0.61,
        },
    ]
}

fn cost_rows() -> Vec<CostRow> {
    let models = [
        ("model-a", 0.042, 0.055),
        ("model-b", 0.031, 0.044),
        ("model-c", 0.018, 0.029),
    ];
    let ages = [
        (Horizon::Day, 1.0, 512_000),
        (Horizon::Week, 1.3, 394_000),
        (Horizon::Month, 1.6, 320_000),
    ];
    ages.iter()
        .flat_map(|&(horizon, scale, surviving_bytes)| {
            models.iter().map(move |&(model, direct, full)| {
                let kb = surviving_bytes as f64 / 1024.0;
                CostRow {
                    model: model.to_string(),
                    horizon,
                    direct_usd: direct * scale * kb,
                    full_usd: full * scale * kb,
                    surviving_bytes,
                    direct_per_kb: direct * scale,
                    full_per_kb: full * scale,
                }
            })
        })
        .collect()
}

fn loss(kind: &str, bytes: i64) -> LossRow {
    LossRow {
        kind: kind.to_string(),
        bytes,
    }
}

impl SurvivalReport {
    pub fn demo() -> SurvivalReport {
        SurvivalReport {
            demo: true,
            totals: Totals {
                spans: 1284,
                bytes_written: 2_516_582,
                bytes_alive: 1_535_000,
            },
            commit_rate: CommitRate {
                spans: 1150,
                bytes_written: 2_250_000,
                bytes_committed: 1_710_000,
                rate: 0.76,
            },
            horizons: AGES.iter().map(horizon_row).collect(),
            loss: LossReport {
                by_who: vec![
                    loss("ai", 460_000),
                    loss("known_human", 380_000),
                    loss("untracked", 160_000),
                ],
                by_how: vec![loss("replace", 710_000), loss("delete", 290_000)],
            },
            slices: slice_groups(),
            examples: example_groups(),
            cost: Some(cost_rows()),
            stats: Some(survival_stats()),
            weightings: weighting_rows(),
            curve_slices: curve_slices(),
            quality: Quality {
                spans: 1284,
                pinned: 1102,
                linked: 874,
                pending: 18,
                unknown: 63,
                none: 329,
            },
        }
    }
}
