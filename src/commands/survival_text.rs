use crate::token_usage::facts_db::SliceDimension;
use crate::token_usage::survival::{Horizon, SurvivalAnchor};
use crate::token_usage::survival_report::{LossRow, SurvivalReport};

const BAR_WIDTH: usize = 20;
const TOO_FEW_SPANS: i64 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct View {
    pub anchor: SurvivalAnchor,
    pub horizon: Horizon,
}

impl View {
    pub fn default_for(report: &SurvivalReport) -> View {
        let horizon = report
            .horizons
            .iter()
            .filter(|row| row.anchor == SurvivalAnchor::Commit)
            .map(|row| row.horizon)
            .max_by_key(|horizon| horizon.seconds())
            .unwrap_or(Horizon::Day);
        View {
            anchor: SurvivalAnchor::Commit,
            horizon,
        }
    }
}

pub fn format_bytes(bytes: i64) -> String {
    const KB: f64 = 1024.0;
    let value = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if value < KB * KB {
        format!("{:.1} KB", value / KB)
    } else if value < KB * KB * KB {
        format!("{:.1} MB", value / (KB * KB))
    } else {
        format!("{:.1} GB", value / (KB * KB * KB))
    }
}

pub fn format_count(count: i64) -> String {
    let digits = count.abs().to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    if count < 0 {
        format!("-{grouped}")
    } else {
        grouped
    }
}

fn percent(fraction: f64) -> String {
    format!("{:.0}%", fraction * 100.0)
}

fn bar(fraction: f64) -> String {
    let filled = (fraction.clamp(0.0, 1.0) * BAR_WIDTH as f64).round() as usize;
    format!("{}{}", "█".repeat(filled), "░".repeat(BAR_WIDTH - filled))
}

fn friendly_label(dimension: SliceDimension, raw: &str) -> String {
    let friendly = match (dimension, raw) {
        (SliceDimension::Relationship, "ai") => "AI code",
        (SliceDimension::Relationship, "known_human") => "human code",
        (SliceDimension::Relationship, "untracked") => "untracked text",
        (SliceDimension::Relationship, "none") => "fresh addition",
        (SliceDimension::EditType, "add") => "added",
        (SliceDimension::EditType, "replace") => "replaced",
        (SliceDimension::Tool, "file_edit") => "file edit",
        _ => raw,
    };
    friendly.to_string()
}

fn friendly_loss(raw: &str) -> String {
    let friendly = match raw {
        "ai" => "AI",
        "known_human" => "known human",
        "replace" => "replaced",
        "delete" => "deleted",
        _ => raw,
    };
    friendly.to_string()
}

fn dimension_title(dimension: SliceDimension) -> &'static str {
    match dimension {
        SliceDimension::Relationship => "what it replaced",
        SliceDimension::EditType => "edit type",
        SliceDimension::Tool => "tool",
        SliceDimension::ModelEffort => "model · effort",
    }
}

fn anchor_title(anchor: SurvivalAnchor) -> &'static str {
    match anchor {
        SurvivalAnchor::Commit => "from commit",
        SurvivalAnchor::Generation => "from generation",
    }
}

struct Writer {
    lines: Vec<String>,
    color: bool,
}

impl Writer {
    fn line(&mut self, text: impl Into<String>) {
        self.lines.push(text.into());
    }

    fn blank(&mut self) {
        self.lines.push(String::new());
    }

    fn heading(&mut self, text: &str) {
        self.blank();
        if self.color {
            self.lines.push(format!("\u{1b}[1m{text}\u{1b}[0m"));
        } else {
            self.lines.push(text.to_string());
        }
    }
}

pub fn render_text(report: &SurvivalReport, view: View, color: bool) -> String {
    let mut out = Writer {
        lines: Vec::new(),
        color,
    };
    title(&mut out, report);
    headline(&mut out, report);
    curve(&mut out, report);
    loss(&mut out, report);
    slices(&mut out, report, view);
    examples(&mut out, report, view);
    cost(&mut out, report);
    quality(&mut out, report, view);
    let mut text = out.lines.join("\n");
    text.push('\n');
    text
}

fn title(out: &mut Writer, report: &SurvivalReport) {
    if out.color {
        out.line("\u{1b}[1mSPAN SURVIVAL ANALYSIS\u{1b}[0m");
    } else {
        out.line("SPAN SURVIVAL ANALYSIS");
    }
    if report.demo {
        out.line("DEMO DATA: these numbers are made up and do not come from your repositories.");
    }
    out.line("How much of the code AI writes is still there later, and why the rest disappears.");
    out.line("A span is a stretch of bytes an AI wrote.");
}

fn headline(out: &mut Writer, report: &SurvivalReport) {
    out.heading("01 HOW MUCH SURVIVES");
    out.line(
        "Of the bytes AI wrote, the share still in the code after 24 hours, 7 days and 30 days. Each age counts only code that old.",
    );
    out.blank();
    out.line(format!(
        "  AI bytes written   {}   ({} spans)",
        format_bytes(report.totals.bytes_written),
        format_count(report.totals.spans)
    ));
    let alive = if report.totals.bytes_written == 0 {
        "–".to_string()
    } else {
        percent(report.totals.bytes_alive as f64 / report.totals.bytes_written as f64)
    };
    out.line(format!("  Still there now    {alive}   (all ages)"));
    out.blank();
    if report.horizons.is_empty() {
        out.line("  No code has reached an age yet. The first row appears 24 hours after code is written.");
        return;
    }
    out.line(format!(
        "  {:<11} {:<5} {:>6} {:>10} {:>10} {:>10}",
        "from", "age", "spans", "surviving", "written", "retention"
    ));
    for row in &report.horizons {
        out.line(format!(
            "  {:<11} {:<5} {:>6} {:>10} {:>10} {:>9.1}%",
            row.anchor.as_str(),
            row.horizon.as_str(),
            format_count(row.spans),
            format_count(row.surviving_bytes),
            format_count(row.total_bytes),
            row.retention * 100.0
        ));
    }
}

fn curve(out: &mut Writer, report: &SurvivalReport) {
    out.heading("02 SURVIVAL CURVE");
    out.line(
        "How the share of AI bytes still present changes as code ages. 'from commit' starts the clock at commit; 'from generation' starts it when the code was written, and includes code never committed.",
    );
    out.blank();
    for anchor in SurvivalAnchor::ALL {
        let points = report.curve(anchor);
        if points.is_empty() {
            out.line(format!("  {}   not yet", anchor_title(anchor)));
            continue;
        }
        out.line(format!("  {}", anchor_title(anchor)));
        for point in points {
            out.line(format!(
                "    {:<5} {} {:>4}",
                point.label,
                bar(point.retention),
                percent(point.retention)
            ));
        }
    }
}

fn loss_block(out: &mut Writer, title: &str, rows: &[LossRow]) {
    out.line(format!("  {title}"));
    let total: i64 = rows.iter().map(|row| row.bytes).sum();
    for row in rows {
        let share = row.bytes as f64 / total as f64;
        out.line(format!(
            "    {:<12} {} {:>4}   {}",
            friendly_loss(&row.kind),
            bar(share),
            percent(share),
            format_bytes(row.bytes)
        ));
    }
}

fn loss(out: &mut Writer, report: &SurvivalReport) {
    out.heading("03 WHY IT DIES");
    out.line(
        "Of the bytes that disappeared, who removed them and how. Human edits only show up when the editor extension is running.",
    );
    out.blank();
    if report.loss.by_who.is_empty() && report.loss.by_how.is_empty() {
        out.line("  No bytes have been removed yet.");
        return;
    }
    loss_block(out, "who removed it", &report.loss.by_who);
    loss_block(out, "how it was removed", &report.loss.by_how);
}

fn slices(out: &mut Writer, report: &SurvivalReport, view: View) {
    out.heading("04 SLICES");
    out.line(
        "Retention at the chosen age, split by how the code was written. Bars are weighted by bytes, so big edits count more. Groups under 10 spans are marked too few.",
    );
    out.line(format!(
        "Showing {} at {}.",
        anchor_title(view.anchor),
        view.horizon.as_str()
    ));
    out.blank();
    let mut shown = false;
    for dimension in SliceDimension::ALL {
        let Some(group) = report.slices.iter().find(|group| {
            group.dimension == dimension
                && group.anchor == view.anchor
                && group.horizon == view.horizon
        }) else {
            continue;
        };
        shown = true;
        out.line(format!("  {}", dimension_title(dimension)));
        for row in &group.rows {
            let few = if row.spans < TOO_FEW_SPANS {
                " too few"
            } else {
                ""
            };
            out.line(format!(
                "    {:<18} {} {:>4}   n={}{}",
                friendly_label(dimension, &row.label),
                bar(row.retention),
                percent(row.retention),
                format_count(row.spans),
                few
            ));
        }
    }
    if !shown {
        out.line("  No slices for this age yet.");
    }
}

fn examples(out: &mut Writer, report: &SurvivalReport, view: View) {
    out.heading("05 BEST AND WORST FILES");
    out.line(
        "Files whose AI code lasted longest and shortest at the chosen age. Files under 1 KB of AI code are left out.",
    );
    out.blank();
    let group = report
        .examples
        .iter()
        .find(|group| group.anchor == view.anchor && group.horizon == view.horizon);
    let Some(group) = group else {
        out.line("  No file has 1 KB of AI code that old yet.");
        return;
    };
    for (tag, files) in [("best", &group.best), ("worst", &group.worst)] {
        for file in files {
            out.line(format!(
                "  {:<6} {:<40} {:>9} {:>5}",
                tag,
                file.file,
                format_bytes(file.total_bytes),
                percent(file.retention)
            ));
        }
    }
}

fn cost(out: &mut Writer, report: &SurvivalReport) {
    out.heading("06 COST PER SURVIVING KB");
    out.line("Dollars spent on a model's calls for each kilobyte of its code that survived.");
    out.blank();
    let Some(rows) = &report.cost else {
        out.line("  Not measured yet.");
        return;
    };
    let most = rows
        .iter()
        .map(|row| row.dollars_per_surviving_kb)
        .fold(0.0_f64, f64::max);
    for row in rows {
        let fraction = if most > 0.0 {
            row.dollars_per_surviving_kb / most
        } else {
            0.0
        };
        out.line(format!(
            "  {:<12} {} ${:.3}",
            row.model,
            bar(fraction),
            row.dollars_per_surviving_kb
        ));
    }
}

fn quality(out: &mut Writer, report: &SurvivalReport, view: View) {
    out.heading("07 WHAT THIS RESTS ON");
    out.line("How many spans each number comes from, and what the data can't see.");
    out.blank();
    let quality = &report.quality;
    for (label, count) in [
        ("captured", quality.spans),
        ("committed", quality.pinned),
        ("model known", quality.linked),
        ("model unknown", quality.unknown),
        ("waiting for a call", quality.pending),
        ("no tool call", quality.none),
    ] {
        out.line(format!("  {label} {}", format_count(count)));
    }
    for row in report
        .horizons
        .iter()
        .filter(|row| row.anchor == view.anchor)
    {
        out.line(format!(
            "  reached {} {}",
            row.horizon.as_str(),
            format_count(row.spans)
        ));
    }
    out.blank();
    out.line(
        "Retention can read too high: a file deleted outside the editor, or edited where no checkpoint fired, still counts as surviving. 'Model unknown' means no matching Claude call was found (a non-Claude agent, or no transcript).",
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token_usage::facts_db::SliceDimension;
    use crate::token_usage::survival_report::{
        CostRow, ExampleGroup, FileRow, HorizonRow, LossReport, LossRow, Quality, SliceGroup,
        SliceRowOut, Totals,
    };

    const ESC: &str = "\u{1b}[";

    fn horizon_row(
        anchor: SurvivalAnchor,
        horizon: Horizon,
        spans: i64,
        surviving: i64,
        total: i64,
    ) -> HorizonRow {
        HorizonRow {
            anchor,
            horizon,
            spans,
            surviving_bytes: surviving,
            total_bytes: total,
            retention: surviving as f64 / total as f64,
        }
    }

    fn slice_row(label: &str, spans: i64, surviving: i64, total: i64) -> SliceRowOut {
        SliceRowOut {
            label: label.to_string(),
            spans,
            surviving_bytes: surviving,
            total_bytes: total,
            retention: surviving as f64 / total as f64,
        }
    }

    fn file_row(file: &str, surviving: i64, total: i64) -> FileRow {
        FileRow {
            repo: "/repo/.git".to_string(),
            file: file.to_string(),
            surviving_bytes: surviving,
            total_bytes: total,
            retention: surviving as f64 / total as f64,
        }
    }

    fn sample_report() -> SurvivalReport {
        SurvivalReport {
            demo: false,
            totals: Totals {
                spans: 1284,
                bytes_written: 2_516_582,
                bytes_alive: 1_535_000,
            },
            horizons: vec![
                horizon_row(SurvivalAnchor::Commit, Horizon::Day, 14, 90, 100),
                horizon_row(SurvivalAnchor::Commit, Horizon::Week, 9, 60, 100),
                horizon_row(SurvivalAnchor::Generation, Horizon::Day, 15, 80, 100),
            ],
            loss: LossReport {
                by_who: vec![
                    LossRow {
                        kind: "ai".to_string(),
                        bytes: 13,
                    },
                    LossRow {
                        kind: "known_human".to_string(),
                        bytes: 5,
                    },
                ],
                by_how: vec![
                    LossRow {
                        kind: "replace".to_string(),
                        bytes: 10,
                    },
                    LossRow {
                        kind: "delete".to_string(),
                        bytes: 8,
                    },
                ],
            },
            slices: vec![
                SliceGroup {
                    dimension: SliceDimension::Relationship,
                    anchor: SurvivalAnchor::Commit,
                    horizon: Horizon::Day,
                    rows: vec![
                        slice_row("known_human", 40, 52, 100),
                        slice_row("none", 3, 71, 100),
                    ],
                },
                SliceGroup {
                    dimension: SliceDimension::Relationship,
                    anchor: SurvivalAnchor::Commit,
                    horizon: Horizon::Week,
                    rows: vec![slice_row("ai", 25, 38, 100)],
                },
            ],
            examples: vec![ExampleGroup {
                anchor: SurvivalAnchor::Commit,
                horizon: Horizon::Day,
                best: vec![file_row("src/good.rs", 1900, 2000)],
                worst: vec![file_row("src/bad.rs", 100, 2000)],
            }],
            cost: None,
            quality: Quality {
                spans: 1284,
                pinned: 1102,
                linked: 874,
                pending: 12,
                unknown: 63,
                none: 335,
            },
        }
    }

    fn day_view() -> View {
        View {
            anchor: SurvivalAnchor::Commit,
            horizon: Horizon::Day,
        }
    }

    fn section(text: &str, number: &str) -> String {
        let mut lines = text.lines();
        let mut out = Vec::new();
        for line in lines.by_ref() {
            if line.starts_with(number) {
                out.push(line);
                break;
            }
        }
        for line in lines {
            let is_heading = line.len() > 2
                && line.as_bytes()[0].is_ascii_digit()
                && line.as_bytes()[1].is_ascii_digit()
                && line.as_bytes()[2] == b' ';
            if is_heading {
                break;
            }
            out.push(line);
        }
        out.join("\n")
    }

    #[test]
    fn bytes_read_in_the_largest_whole_unit() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(900), "900 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(2_516_582), "2.4 MB");
    }

    #[test]
    fn counts_get_thousands_separators() {
        assert_eq!(format_count(7), "7");
        assert_eq!(format_count(1284), "1,284");
        assert_eq!(format_count(1_000_000), "1,000,000");
    }

    #[test]
    fn the_default_view_is_commit_at_the_longest_age_with_data() {
        assert_eq!(
            View::default_for(&sample_report()),
            View {
                anchor: SurvivalAnchor::Commit,
                horizon: Horizon::Week
            }
        );
    }

    #[test]
    fn the_default_view_without_data_is_commit_at_the_first_age() {
        assert_eq!(View::default_for(&SurvivalReport::empty()), day_view());
    }

    #[test]
    fn the_default_view_stays_on_commit_even_if_generation_has_longer_data() {
        let mut report = SurvivalReport::empty();
        report.horizons = vec![
            horizon_row(SurvivalAnchor::Commit, Horizon::Day, 1, 1, 1),
            horizon_row(SurvivalAnchor::Generation, Horizon::Month, 1, 1, 1),
        ];

        assert_eq!(View::default_for(&report), day_view());
    }

    #[test]
    fn the_report_opens_with_the_title_and_what_a_span_is() {
        let text = render_text(&sample_report(), day_view(), false);

        assert!(text.starts_with("SPAN SURVIVAL ANALYSIS\n"), "{text}");
        assert!(text.contains(
            "How much of the code AI writes is still there later, and why the rest disappears."
        ));
        assert!(text.contains("A span is a stretch of bytes an AI wrote."));
    }

    #[test]
    fn the_sections_appear_in_order() {
        let text = render_text(&sample_report(), day_view(), false);

        let headings = [
            "01 HOW MUCH SURVIVES",
            "02 SURVIVAL CURVE",
            "03 WHY IT DIES",
            "04 SLICES",
            "05 BEST AND WORST FILES",
            "06 COST PER SURVIVING KB",
            "07 WHAT THIS RESTS ON",
        ];
        let positions: Vec<usize> = headings
            .iter()
            .map(|heading| text.find(heading).unwrap_or_else(|| panic!("{heading}")))
            .collect();
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{text}");
    }

    #[test]
    fn an_empty_report_says_nothing_has_matured_and_still_shows_every_section() {
        let text = render_text(&SurvivalReport::empty(), day_view(), false);

        assert!(
            section(&text, "01").contains("No code has reached an age yet."),
            "{text}"
        );
        assert!(section(&text, "03").contains("No bytes have been removed yet."));
        assert!(section(&text, "05").contains("No file has"));
        assert!(section(&text, "06").contains("Not measured yet"));
        assert!(section(&text, "07").contains("captured"));
    }

    #[test]
    fn the_headline_shows_bytes_written_and_what_is_still_there() {
        let text = render_text(&sample_report(), day_view(), false);
        let headline = section(&text, "01");

        assert!(headline.contains("AI bytes written"), "{headline}");
        assert!(headline.contains("2.4 MB"), "{headline}");
        assert!(headline.contains("1,284 spans"), "{headline}");
        assert!(headline.contains("Still there now"), "{headline}");
        assert!(headline.contains("61%"), "{headline}");
        assert!(headline.contains("all ages"), "{headline}");
        assert!(headline.contains("Each age counts only code that old."));
    }

    #[test]
    fn the_headline_table_has_one_row_per_measured_age_and_anchor() {
        let text = render_text(&sample_report(), day_view(), false);
        let headline = section(&text, "01");

        let rows: Vec<Vec<&str>> = headline
            .lines()
            .map(|line| line.split_whitespace().collect::<Vec<_>>())
            .filter(|tokens| matches!(tokens.first(), Some(&"commit") | Some(&"generation")))
            .collect();
        assert_eq!(rows.len(), 3, "{headline}");
        assert_eq!(rows[0], ["commit", "24h", "14", "90", "100", "90.0%"]);
        assert_eq!(rows[1], ["commit", "7d", "9", "60", "100", "60.0%"]);
        assert_eq!(rows[2], ["generation", "24h", "15", "80", "100", "80.0%"]);
    }

    #[test]
    fn the_curve_shows_each_anchor_from_full_retention() {
        let text = render_text(&sample_report(), day_view(), false);
        let curve = section(&text, "02");

        assert!(curve.contains("from commit"), "{curve}");
        assert!(curve.contains("from generation"), "{curve}");
        assert!(curve.contains("100%"), "{curve}");
        assert!(curve.contains("90%"), "{curve}");
        assert!(curve.contains("60%"), "{curve}");
        assert!(curve.contains("█"), "{curve}");
    }

    #[test]
    fn the_curve_says_not_yet_when_there_is_no_data() {
        let text = render_text(&SurvivalReport::empty(), day_view(), false);

        assert!(section(&text, "02").contains("not yet"), "{text}");
    }

    #[test]
    fn why_it_dies_names_who_and_how_with_shares_of_removed_bytes() {
        let text = render_text(&sample_report(), day_view(), false);
        let loss = section(&text, "03");

        assert!(loss.contains("who removed it"), "{loss}");
        assert!(loss.contains("how it was removed"), "{loss}");
        for expected in [
            ("AI", "72%"),
            ("known human", "28%"),
            ("replaced", "56%"),
            ("deleted", "44%"),
        ] {
            let line = loss
                .lines()
                .find(|line| line.contains(expected.0))
                .unwrap_or_else(|| panic!("{}", expected.0));
            assert!(line.contains(expected.1), "{line}");
        }
    }

    #[test]
    fn slices_show_only_the_chosen_age_and_anchor_with_friendly_labels() {
        let text = render_text(&sample_report(), day_view(), false);
        let slices = section(&text, "04");

        assert!(slices.contains("human code"), "{slices}");
        assert!(slices.contains("fresh addition"), "{slices}");
        assert!(slices.contains("52%"), "{slices}");
        assert!(!slices.contains("AI code"), "{slices}");
        assert!(slices.contains("Groups under 10 spans are marked too few."));
    }

    #[test]
    fn small_groups_are_marked_too_few() {
        let text = render_text(&sample_report(), day_view(), false);
        let slices = section(&text, "04");

        let few = slices
            .lines()
            .find(|line| line.contains("fresh addition"))
            .unwrap();
        let many = slices
            .lines()
            .find(|line| line.contains("human code"))
            .unwrap();
        assert!(few.contains("n=3"), "{few}");
        assert!(few.contains("too few"), "{few}");
        assert!(many.contains("n=40"), "{many}");
        assert!(!many.contains("too few"), "{many}");
    }

    #[test]
    fn the_examples_list_best_and_worst_files_for_the_chosen_age() {
        let text = render_text(&sample_report(), day_view(), false);
        let examples = section(&text, "05");

        let best = examples
            .lines()
            .find(|line| line.contains("src/good.rs"))
            .unwrap();
        let worst = examples
            .lines()
            .find(|line| line.contains("src/bad.rs"))
            .unwrap();
        assert!(best.contains("best"), "{best}");
        assert!(best.contains("95%"), "{best}");
        assert!(worst.contains("worst"), "{worst}");
        assert!(worst.contains("5%"), "{worst}");
    }

    #[test]
    fn cost_says_not_measured_yet_until_there_is_data() {
        let text = render_text(&sample_report(), day_view(), false);

        assert!(section(&text, "06").contains("Not measured yet"), "{text}");
    }

    #[test]
    fn cost_rows_show_dollars_per_surviving_kilobyte() {
        let mut report = sample_report();
        report.cost = Some(vec![CostRow {
            model: "model-a".to_string(),
            dollars_per_surviving_kb: 0.042,
        }]);

        let text = render_text(&report, day_view(), false);

        let cost = section(&text, "06");
        assert!(cost.contains("model-a"), "{cost}");
        assert!(cost.contains("$0.042"), "{cost}");
        assert!(!cost.contains("Not measured yet"), "{cost}");
    }

    #[test]
    fn data_quality_lists_counts_and_the_overstatement_warning() {
        let text = render_text(&sample_report(), day_view(), false);
        let quality = section(&text, "07");

        for expected in [
            "captured 1,284",
            "committed 1,102",
            "model known 874",
            "model unknown 63",
        ] {
            assert!(quality.contains(expected), "{expected}: {quality}");
        }
        assert!(quality.contains("Retention can read too high"), "{quality}");
    }

    #[test]
    fn a_demo_report_says_so_at_the_top() {
        let mut report = sample_report();
        report.demo = true;

        let text = render_text(&report, day_view(), false);

        assert!(text.contains("DEMO DATA"), "{text}");
        assert!(!render_text(&sample_report(), day_view(), false).contains("DEMO DATA"));
    }

    #[test]
    fn colour_codes_appear_only_when_asked_for() {
        let plain = render_text(&sample_report(), day_view(), false);
        let coloured = render_text(&sample_report(), day_view(), true);

        assert!(!plain.contains(ESC));
        assert!(coloured.contains(ESC));
    }

    #[test]
    fn the_demo_report_reads_well_as_text() {
        let demo = SurvivalReport::demo();

        insta::assert_snapshot!(render_text(&demo, View::default_for(&demo), false));
    }
}
