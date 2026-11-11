const DAY_SECS: i64 = 24 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Horizon {
    Day,
    Week,
    Month,
}

impl Horizon {
    pub const ALL: [Horizon; 3] = [Horizon::Day, Horizon::Week, Horizon::Month];

    pub fn as_str(self) -> &'static str {
        match self {
            Horizon::Day => "24h",
            Horizon::Week => "7d",
            Horizon::Month => "30d",
        }
    }

    pub fn seconds(self) -> i64 {
        match self {
            Horizon::Day => DAY_SECS,
            Horizon::Week => 7 * DAY_SECS,
            Horizon::Month => 30 * DAY_SECS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurvivalAnchor {
    Commit,
    Generation,
}

impl SurvivalAnchor {
    pub const ALL: [SurvivalAnchor; 2] = [SurvivalAnchor::Commit, SurvivalAnchor::Generation];

    pub fn as_str(self) -> &'static str {
        match self {
            SurvivalAnchor::Commit => "commit",
            SurvivalAnchor::Generation => "generation",
        }
    }
}

pub fn horizons_reached(age_secs: i64) -> Vec<Horizon> {
    Horizon::ALL
        .into_iter()
        .filter(|horizon| age_secs >= horizon.seconds())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_are_named_for_storage_and_the_report() {
        assert_eq!(SurvivalAnchor::Commit.as_str(), "commit");
        assert_eq!(SurvivalAnchor::Generation.as_str(), "generation");
    }

    #[test]
    fn a_young_span_has_reached_no_horizon() {
        assert_eq!(horizons_reached(2 * 60 * 60), Vec::<Horizon>::new());
    }

    #[test]
    fn a_span_exactly_one_day_old_has_reached_the_first_horizon() {
        assert_eq!(horizons_reached(DAY_SECS), vec![Horizon::Day]);
    }

    #[test]
    fn a_two_day_old_span_has_reached_only_the_first_horizon() {
        assert_eq!(horizons_reached(2 * DAY_SECS), vec![Horizon::Day]);
    }

    #[test]
    fn a_span_past_thirty_days_keeps_every_horizon() {
        assert_eq!(horizons_reached(45 * DAY_SECS), Horizon::ALL.to_vec());
    }

    #[test]
    fn a_negative_age_from_clock_skew_has_reached_no_horizon() {
        assert_eq!(horizons_reached(-60), Vec::<Horizon>::new());
    }

    #[test]
    fn horizon_labels_match_the_report_columns() {
        let labels: Vec<&str> = Horizon::ALL
            .iter()
            .map(|horizon| horizon.as_str())
            .collect();
        assert_eq!(labels, vec!["24h", "7d", "30d"]);
    }
}
