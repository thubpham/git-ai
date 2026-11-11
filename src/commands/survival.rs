use crate::token_usage::facts_db::{FactsDatabase, HorizonRetention};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn handle_survival(args: &[String]) {
    if let Some(arg) = args.first() {
        eprintln!("Unknown argument: {}", arg);
        std::process::exit(1);
    }
    match run() {
        Ok(report) => print!("{}", render(&report)),
        Err(message) => {
            eprintln!("error: {}", message);
            std::process::exit(1);
        }
    }
}

fn run() -> Result<Vec<HorizonRetention>, String> {
    let path = crate::config::internal_dir_path()
        .ok_or_else(|| "could not locate the git-ai internal directory".to_string())?
        .join("transcript-facts-db");
    let db = FactsDatabase::open(path).map_err(|e| e.to_string())?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs() as i64;
    db.measure_due_spans(now).map_err(|e| e.to_string())?;
    db.survival_report().map_err(|e| e.to_string())
}

fn render(report: &[HorizonRetention]) -> String {
    if report.is_empty() {
        return "No AI-written spans have reached a survival horizon yet.\n".to_string();
    }
    let mut out = format!(
        "{:<12} {:<8} {:>6} {:>10} {:>10} {:>10}\n",
        "anchor", "horizon", "spans", "surviving", "total", "retention"
    );
    for row in report {
        out.push_str(&format!(
            "{:<12} {:<8} {:>6} {:>10} {:>10} {:>9.1}%\n",
            row.anchor.as_str(),
            row.horizon.as_str(),
            row.spans,
            row.surviving_bytes,
            row.total_bytes,
            row.retention() * 100.0
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token_usage::survival::{Horizon, SurvivalAnchor};

    #[test]
    fn an_empty_report_says_nothing_has_matured() {
        assert_eq!(
            render(&[]),
            "No AI-written spans have reached a survival horizon yet.\n"
        );
    }

    #[test]
    fn a_row_shows_pooled_bytes_and_retention() {
        let rendered = render(&[HorizonRetention {
            anchor: SurvivalAnchor::Commit,
            horizon: Horizon::Day,
            spans: 2,
            surviving_bytes: 90,
            total_bytes: 100,
        }]);

        let row = rendered.lines().nth(1).unwrap();
        assert_eq!(
            row.split_whitespace().collect::<Vec<_>>(),
            ["commit", "24h", "2", "90", "100", "90.0%"]
        );
    }
}
