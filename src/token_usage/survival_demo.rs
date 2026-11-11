use super::facts_db::SliceDimension;
use super::survival::{Horizon, SurvivalAnchor};
use super::survival_report::{
    CostRow, ExampleGroup, FileRow, HorizonRow, LossReport, LossRow, Quality, SliceGroup,
    SliceRowOut, SurvivalReport, Totals,
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
            cost: Some(vec![
                CostRow {
                    model: "model-a".to_string(),
                    dollars_per_surviving_kb: 0.042,
                },
                CostRow {
                    model: "model-b".to_string(),
                    dollars_per_surviving_kb: 0.031,
                },
                CostRow {
                    model: "model-c".to_string(),
                    dollars_per_surviving_kb: 0.018,
                },
            ]),
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
