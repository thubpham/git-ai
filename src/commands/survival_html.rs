use crate::commands::survival_text::View;
use crate::token_usage::survival_report::SurvivalReport;

const PAGE: &str = include_str!("survival_page.html");
const DEMO_BANNER: &str = "<div class=\"demo\">DEMO DATA: these numbers are made up and do not come from your repositories.</div>";

pub fn render_html(report: &SurvivalReport, view: View) -> String {
    let report_json = serde_json::to_string(report)
        .unwrap_or_else(|_| "null".to_string())
        .replace('<', "\\u003c");
    let view_json = format!(
        "{{\"anchor\":\"{}\",\"horizon\":\"{}\"}}",
        view.anchor.as_str(),
        view.horizon.as_str()
    );
    PAGE.replace(
        "__DEMO_BANNER__",
        if report.demo { DEMO_BANNER } else { "" },
    )
    .replace("__VIEW_JSON__", &view_json)
    .replace("__REPORT_JSON__", &report_json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token_usage::survival::{Horizon, SurvivalAnchor};
    use crate::token_usage::survival_report::{ExampleGroup, FileRow};

    const DATA_OPEN: &str = "<script id=\"report\" type=\"application/json\">";

    fn day_view() -> View {
        View {
            anchor: SurvivalAnchor::Commit,
            horizon: Horizon::Day,
        }
    }

    fn embedded_report(html: &str) -> SurvivalReport {
        let start = html.find(DATA_OPEN).expect("data block") + DATA_OPEN.len();
        let end = start + html[start..].find("</script>").expect("data block end");
        serde_json::from_str(&html[start..end]).unwrap()
    }

    #[test]
    fn the_embedded_data_parses_back_to_the_report() {
        let demo = SurvivalReport::demo();

        let html = render_html(&demo, day_view());

        assert_eq!(embedded_report(&html), demo);
    }

    #[test]
    fn a_path_with_a_script_tag_cannot_end_the_data_block() {
        let mut report = SurvivalReport::empty();
        let hostile = "</script><img src=x onerror=alert(1)>.rs";
        report.examples.push(ExampleGroup {
            anchor: SurvivalAnchor::Commit,
            horizon: Horizon::Day,
            best: vec![FileRow {
                repo: "/repo/.git".to_string(),
                file: hostile.to_string(),
                surviving_bytes: 1,
                total_bytes: 2,
                retention: 0.5,
            }],
            worst: Vec::new(),
        });

        let html = render_html(&report, day_view());

        assert!(!html.contains(hostile));
        assert_eq!(embedded_report(&html).examples[0].best[0].file, hostile);
    }

    #[test]
    fn the_page_loads_nothing_from_the_network() {
        let html = render_html(&SurvivalReport::demo(), day_view());

        assert!(!html.contains("http://"));
        assert!(!html.contains("https://"));
    }

    #[test]
    fn the_demo_banner_appears_only_for_demo_data() {
        let demo = render_html(&SurvivalReport::demo(), day_view());
        let real = render_html(&SurvivalReport::empty(), day_view());

        assert!(demo.contains("DEMO DATA"));
        assert!(!real.contains("DEMO DATA"));
    }
}
