use std::collections::BTreeMap;

use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};

use super::facts_db::SpanHistory;

pub const DAY_SECS: f64 = 86_400.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Lifetime {
    pub days: f64,
    pub died: bool,
    pub session_id: String,
}

pub fn lifetime(history: &SpanHistory, threshold: f64, now: i64) -> Lifetime {
    let deadly = threshold * history.committed_bytes as f64;
    let mut lost = 0;
    let death = history
        .losses
        .iter()
        .find_map(|&(touched_at, bytes_removed)| {
            lost += bytes_removed;
            (lost as f64 >= deadly).then_some(touched_at)
        });
    let end = death.unwrap_or(now);
    Lifetime {
        days: (end - history.pinned_at).max(0) as f64 / DAY_SECS,
        died: death.is_some(),
        session_id: history.session_id.clone(),
    }
}

pub fn kaplan_meier(lifetimes: &[Lifetime]) -> Vec<(f64, f64)> {
    let mut death_times: Vec<f64> = lifetimes
        .iter()
        .filter(|lifetime| lifetime.died)
        .map(|lifetime| lifetime.days)
        .collect();
    death_times.sort_by(f64::total_cmp);
    death_times.dedup();
    let mut survival = 1.0;
    death_times
        .into_iter()
        .map(|time| {
            let at_risk = lifetimes
                .iter()
                .filter(|lifetime| lifetime.days >= time)
                .count();
            let deaths = lifetimes
                .iter()
                .filter(|lifetime| lifetime.died && lifetime.days == time)
                .count();
            survival *= 1.0 - deaths as f64 / at_risk as f64;
            (time, survival)
        })
        .collect()
}

pub fn survival_at(curve: &[(f64, f64)], day: f64) -> f64 {
    curve
        .iter()
        .take_while(|(time, _)| *time <= day)
        .last()
        .map_or(1.0, |&(_, survival)| survival)
}

pub fn rmst(curve: &[(f64, f64)], limit: f64) -> f64 {
    let mut area = 0.0;
    let mut previous_time = 0.0;
    let mut survival = 1.0;
    for &(time, next_survival) in curve {
        if time >= limit {
            break;
        }
        area += survival * (time - previous_time);
        previous_time = time;
        survival = next_survival;
    }
    area + survival * (limit - previous_time)
}

pub fn session_bootstrap(
    lifetimes: &[Lifetime],
    resamples: usize,
    seed: u64,
    statistic: impl Fn(&[Lifetime]) -> f64,
) -> (f64, f64) {
    let mut sessions: BTreeMap<&str, Vec<Lifetime>> = BTreeMap::new();
    for lifetime in lifetimes {
        sessions
            .entry(lifetime.session_id.as_str())
            .or_default()
            .push(lifetime.clone());
    }
    let sessions: Vec<Vec<Lifetime>> = sessions.into_values().collect();
    let mut rng = StdRng::seed_from_u64(seed);
    let mut values: Vec<f64> = (0..resamples)
        .map(|_| {
            let sample: Vec<Lifetime> = (0..sessions.len())
                .flat_map(|_| {
                    sessions[rng.random_range(0..sessions.len())]
                        .iter()
                        .cloned()
                })
                .collect();
            statistic(&sample)
        })
        .collect();
    values.sort_by(f64::total_cmp);
    let at = |quantile: f64| values[((values.len() - 1) as f64 * quantile).round() as usize];
    (at(0.025), at(0.975))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn life(days: f64, died: bool, session_id: &str) -> Lifetime {
        Lifetime {
            days,
            died,
            session_id: session_id.to_string(),
        }
    }

    fn history(losses: Vec<(i64, i64)>) -> SpanHistory {
        SpanHistory {
            session_id: "s1".to_string(),
            pinned_at: 1000,
            committed_bytes: 100,
            losses,
        }
    }

    const DAY: i64 = 86_400;

    #[test]
    fn a_span_dies_at_the_loss_that_crosses_the_threshold() {
        let span = history(vec![(1000 + DAY, 30), (1000 + 3 * DAY, 30)]);

        assert_eq!(lifetime(&span, 0.5, 1000 + 10 * DAY), life(3.0, true, "s1"));
    }

    #[test]
    fn a_span_below_the_threshold_is_censored_at_report_time() {
        let span = history(vec![(1000 + DAY, 30), (1000 + 3 * DAY, 30)]);

        assert_eq!(
            lifetime(&span, 1.0, 1000 + 10 * DAY),
            life(10.0, false, "s1")
        );
    }

    #[test]
    fn kaplan_meier_drops_at_each_death_and_keeps_censored_spans_at_risk_until_they_leave() {
        let lifetimes = vec![
            life(1.0, true, "a"),
            life(2.0, true, "b"),
            life(2.0, false, "c"),
            life(3.0, true, "d"),
        ];

        let curve = kaplan_meier(&lifetimes);

        assert_eq!(curve.len(), 3);
        assert!((curve[0].0 - 1.0).abs() < 1e-12 && (curve[0].1 - 0.75).abs() < 1e-12);
        assert!((curve[1].0 - 2.0).abs() < 1e-12 && (curve[1].1 - 0.5).abs() < 1e-12);
        assert!((curve[2].0 - 3.0).abs() < 1e-12 && curve[2].1.abs() < 1e-12);
    }

    #[test]
    fn survival_at_a_day_is_the_last_step_at_or_before_it() {
        let curve = vec![(1.0, 0.75), (2.0, 0.5), (3.0, 0.0)];

        assert!((survival_at(&curve, 0.5) - 1.0).abs() < 1e-12);
        assert!((survival_at(&curve, 1.0) - 0.75).abs() < 1e-12);
        assert!((survival_at(&curve, 2.5) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn rmst_is_the_area_under_the_curve_up_to_the_limit() {
        let curve = vec![(1.0, 0.75), (2.0, 0.5), (3.0, 0.0)];

        assert!((rmst(&curve, 2.5) - 2.0).abs() < 1e-12);
        assert!((rmst(&curve, 10.0) - 2.25).abs() < 1e-12);
    }

    #[test]
    fn a_single_session_has_no_spread_between_resamples() {
        let lifetimes = vec![life(1.0, true, "s1"), life(4.0, false, "s1")];

        let (low, high) = session_bootstrap(&lifetimes, 200, 7, |sample| {
            survival_at(&kaplan_meier(sample), 2.0)
        });

        assert!((low - 0.5).abs() < 1e-12);
        assert!((high - 0.5).abs() < 1e-12);
    }

    #[test]
    fn the_same_seed_gives_the_same_range() {
        let lifetimes = vec![
            life(1.0, true, "s1"),
            life(5.0, false, "s2"),
            life(2.0, true, "s3"),
            life(6.0, false, "s4"),
        ];
        let statistic = |sample: &[Lifetime]| rmst(&kaplan_meier(sample), 4.0);

        let first = session_bootstrap(&lifetimes, 300, 11, statistic);
        let second = session_bootstrap(&lifetimes, 300, 11, statistic);

        assert_eq!(first, second);
        assert!(first.0 <= first.1);
    }
}
