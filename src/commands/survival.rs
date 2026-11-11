use crate::commands::survival_html::render_html;
use crate::commands::survival_text::{View, render_text};
use crate::token_usage::facts_db::FactsDatabase;
use crate::token_usage::survival::{Horizon, SurvivalAnchor};
use crate::token_usage::survival_report::SurvivalReport;
use std::io::IsTerminal;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub json: bool,
    pub html: Option<String>,
    pub demo: bool,
    pub measure: bool,
    pub age: Option<Horizon>,
    pub from: Option<SurvivalAnchor>,
}

fn value_of<'a>(args: &'a [String], index: usize, flag: &str) -> Result<&'a str, String> {
    args.get(index + 1)
        .map(String::as_str)
        .ok_or_else(|| format!("Missing value for {flag}."))
}

pub fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut options = Options {
        json: false,
        html: None,
        demo: false,
        measure: true,
        age: None,
        from: None,
    };
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => options.json = true,
            "--demo" => options.demo = true,
            "--no-measure" => options.measure = false,
            "--html" => {
                options.html = Some(value_of(args, index, "--html")?.to_string());
                index += 1;
            }
            "--age" => {
                options.age = Some(match value_of(args, index, "--age")? {
                    "24h" => Horizon::Day,
                    "7d" => Horizon::Week,
                    "30d" => Horizon::Month,
                    other => {
                        return Err(format!(
                            "Invalid --age value: {other}. Expected 24h, 7d or 30d."
                        ));
                    }
                });
                index += 1;
            }
            "--from" => {
                options.from = Some(match value_of(args, index, "--from")? {
                    "commit" => SurvivalAnchor::Commit,
                    "generation" => SurvivalAnchor::Generation,
                    other => {
                        return Err(format!(
                            "Invalid --from value: {other}. Expected commit or generation."
                        ));
                    }
                });
                index += 1;
            }
            other => return Err(format!("Unknown argument: {other}")),
        }
        index += 1;
    }
    Ok(options)
}

pub fn handle_survival(args: &[String]) {
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    };
    let report = match load_report(&options) {
        Ok(report) => report,
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(1);
        }
    };
    let mut view = View::default_for(&report);
    if let Some(anchor) = options.from {
        view.anchor = anchor;
    }
    if let Some(horizon) = options.age {
        view.horizon = horizon;
    }
    if let Some(path) = &options.html {
        match std::fs::write(path, render_html(&report, view)) {
            Ok(()) => println!("Wrote {path}"),
            Err(error) => {
                eprintln!("error: could not write {path}: {error}");
                std::process::exit(1);
            }
        }
        return;
    }
    if options.json {
        match serde_json::to_string_pretty(&report) {
            Ok(json) => println!("{json}"),
            Err(error) => {
                eprintln!("error: {error}");
                std::process::exit(1);
            }
        }
        return;
    }
    let color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    print!("{}", render_text(&report, view, color));
}

fn load_report(options: &Options) -> Result<SurvivalReport, String> {
    if options.demo {
        return Ok(SurvivalReport::demo());
    }
    let path = crate::config::internal_dir_path()
        .ok_or_else(|| "could not locate the git-ai internal directory".to_string())?
        .join("transcript-facts-db");
    let db = FactsDatabase::open(path).map_err(|e| e.to_string())?;
    if options.measure {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_secs() as i64;
        db.measure_due_spans(now).map_err(|e| e.to_string())?;
    }
    SurvivalReport::build(&db).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn no_arguments_measure_and_print_the_text_report() {
        assert_eq!(
            parse_args(&[]).unwrap(),
            Options {
                json: false,
                html: None,
                demo: false,
                measure: true,
                age: None,
                from: None,
            }
        );
    }

    #[test]
    fn each_flag_sets_its_option() {
        let options = parse_args(&args(&[
            "--json",
            "--html",
            "out.html",
            "--demo",
            "--no-measure",
            "--age",
            "7d",
            "--from",
            "generation",
        ]))
        .unwrap();

        assert_eq!(
            options,
            Options {
                json: true,
                html: Some("out.html".to_string()),
                demo: true,
                measure: false,
                age: Some(Horizon::Week),
                from: Some(SurvivalAnchor::Generation),
            }
        );
    }

    #[test]
    fn every_age_name_is_accepted() {
        for (name, horizon) in [
            ("24h", Horizon::Day),
            ("7d", Horizon::Week),
            ("30d", Horizon::Month),
        ] {
            let options = parse_args(&args(&["--age", name])).unwrap();
            assert_eq!(options.age, Some(horizon));
        }
    }

    #[test]
    fn bad_arguments_are_rejected_with_a_reason() {
        for bad in [
            vec!["--html"],
            vec!["--age"],
            vec!["--age", "3d"],
            vec!["--from"],
            vec!["--from", "tomorrow"],
            vec!["--nope"],
        ] {
            assert!(parse_args(&args(&bad)).is_err(), "{bad:?}");
        }
    }
}
