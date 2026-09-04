//! The `--timings` page.
//!
//! One string, no fetches, integer arithmetic only. Percentages are computed in basis points and
//! printed as two decimals rather than going through `f64`, so the page a run produces is a
//! function of its numbers and nothing else.

use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use core::fmt::{Arguments, Write as _};

use crate::{
    event::{Activity, Outcome},
    timeline::{ReportMeta, Timeline},
};

/// The width of the concurrency plot's coordinate system. The plot scales to its container; the
/// numbers below it are HTML, so nothing stretches.
const PLOT_WIDTH: u64 = 1000;
const PLOT_HEIGHT: u64 = 160;
/// How many points the concurrency plot samples. Enough to show the shape of a build, few enough
/// that the page stays small.
const PLOT_SAMPLES: usize = 240;

/// The page under construction.
pub(crate) struct Page {
    out: String,
}

impl Page {
    /// Render one run's ledger.
    pub(crate) fn render(timeline: &Timeline, meta: &ReportMeta) -> String {
        // A run with no reported work still gets a page: "nothing was reported" is an answer, and a
        // zero denominator is not.
        let total = meta
            .total_micros
            .max(
                timeline
                    .spans()
                    .iter()
                    .map(|span| span.end_micros)
                    .max()
                    .unwrap_or(0),
            )
            .max(1);
        let mut page = Self {
            out: String::with_capacity(4096),
        };
        page.head(meta);
        page.summary(timeline, meta, total);
        page.gantt(timeline, total);
        page.concurrency(timeline, total);
        page.tail();
        page.out
    }

    fn head(&mut self, meta: &ReportMeta) {
        self.put(format_args!(
            "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
             <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
             <title>jals timings — {}</title>\n<style>{STYLE}</style>\n</head>\n<body>\n",
            Self::escaped(meta.project.as_deref().unwrap_or("jals"))
        ));
        self.put(format_args!(
            "<header>\n<h1>jals timings</h1>\n<p class=\"cmd\">{}</p>\n</header>\n",
            Self::escaped(&meta.command)
        ));
    }

    fn summary(&mut self, timeline: &Timeline, meta: &ReportMeta, total: u64) {
        let ran = timeline
            .spans()
            .iter()
            .filter(|span| span.outcome.ran())
            .count();
        let fresh = timeline
            .spans()
            .iter()
            .filter(|span| span.outcome == Outcome::Fresh)
            .count();
        self.out.push_str("<section class=\"cards\">\n");
        self.card("total", &Self::duration(meta.total_micros.max(total)));
        self.card("units", &timeline.spans().len().to_string());
        self.card("ran", &ran.to_string());
        self.card("fresh", &fresh.to_string());
        self.out.push_str("</section>\n");

        self.out.push_str(
            "<section>\n<h2>by activity</h2>\n<table>\n\
             <thead><tr><th>activity</th><th>units</th><th>time</th><th></th></tr></thead>\n\
             <tbody>\n",
        );
        let by_activity = timeline.by_activity();
        let widest = by_activity
            .iter()
            .map(|(_, time, _)| *time)
            .max()
            .unwrap_or(1)
            .max(1);
        for (activity, time, units) in &by_activity {
            self.put(format_args!(
                "<tr><td><span class=\"dot\" style=\"background:{}\"></span>{}</td>\
                 <td class=\"num\">{units}</td><td class=\"num\">{}</td>\
                 <td class=\"track\"><span style=\"width:{}%;background:{}\"></span></td></tr>\n",
                Self::colour(*activity),
                activity.label(),
                Self::duration(*time),
                Self::percent(*time, widest),
                Self::colour(*activity),
            ));
        }
        self.out.push_str("</tbody>\n</table>\n</section>\n");
    }

    fn gantt(&mut self, timeline: &Timeline, total: u64) {
        self.out
            .push_str("<section>\n<h2>timeline</h2>\n<div class=\"gantt\">\n");
        if timeline.spans().is_empty() {
            self.out
                .push_str("<p class=\"empty\">nothing reported any work.</p>\n");
        }
        let mut ordered: Vec<&crate::timeline::Span> = timeline.spans().iter().collect();
        ordered.sort_by_key(|span| (span.start_micros, span.end_micros));
        for span in ordered {
            let width = Self::percent(span.duration_micros().max(total / 400), total);
            self.put(format_args!(
                "<div class=\"row\" title=\"{} — {}\">\
                 <div class=\"label\">{}</div>\
                 <div class=\"lane\"><span class=\"bar {}\" style=\"left:{}%;width:{width}%;background:{}\"></span></div>\
                 <div class=\"time\">{}</div></div>\n",
                Self::escaped(&span.unit.describe()),
                Self::escaped(span.unit.activity.label()),
                Self::escaped(&span.unit.describe()),
                Self::outcome_class(span.outcome),
                Self::percent(span.start_micros, total),
                Self::colour(span.unit.activity),
                Self::duration(span.duration_micros()),
            ));
        }
        self.out.push_str("</div>\n</section>\n");
    }

    fn concurrency(&mut self, timeline: &Timeline, total: u64) {
        let samples = timeline.concurrency(total, PLOT_SAMPLES);
        let peak = samples.iter().copied().max().unwrap_or(0).max(1) as u64;
        self.put(format_args!(
            "<section>\n<h2>units in flight</h2>\n\
             <svg class=\"plot\" viewBox=\"0 0 {PLOT_WIDTH} {PLOT_HEIGHT}\" preserveAspectRatio=\"none\" role=\"img\" aria-label=\"units running concurrently over time\">\n\
             <polyline points=\""
        ));
        for (index, active) in samples.iter().enumerate() {
            let x = PLOT_WIDTH * index as u64 / PLOT_SAMPLES.max(1) as u64;
            let y = PLOT_HEIGHT - PLOT_HEIGHT * *active as u64 / peak;
            self.put(format_args!("{x},{y} "));
        }
        self.put(format_args!(
            "\"/>\n</svg>\n<p class=\"axis\"><span>0</span><span>peak {peak}</span><span>{}</span></p>\n</section>\n",
            Self::duration(total)
        ));
    }

    fn tail(&mut self) {
        self.out.push_str("</body>\n</html>\n");
    }

    fn card(&mut self, label: &str, value: &str) {
        self.put(format_args!(
            "<div class=\"card\"><span class=\"v\">{}</span><span class=\"k\">{label}</span></div>\n",
            Self::escaped(value)
        ));
    }

    /// `value / basis` as a percentage with two decimals, in integer arithmetic.
    fn percent(value: u64, basis: u64) -> String {
        let points = value.saturating_mul(10_000) / basis.max(1);
        let mut rendered = String::new();
        let _ = write!(rendered, "{}.{:02}", points / 100, points % 100);
        rendered
    }

    /// A duration a person reads: `1.234s` above a second, `85ms` below one.
    fn duration(micros: u64) -> String {
        let mut rendered = String::new();
        if micros >= 1_000_000 {
            let _ = write!(
                rendered,
                "{}.{:03}s",
                micros / 1_000_000,
                micros % 1_000_000 / 1_000
            );
        } else {
            let _ = write!(rendered, "{}ms", micros / 1_000);
        }
        rendered
    }

    /// One colour per activity, so a row's kind is readable without reading its label.
    const fn colour(activity: Activity) -> &'static str {
        match activity {
            Activity::Script => "#8b7cd8",
            Activity::Resolve => "#6c8cd5",
            Activity::Fetch => "#3f9dd4",
            Activity::Extract => "#3fb0a5",
            Activity::Remap => "#4fae63",
            Activity::Merge => "#7cae4f",
            Activity::Decompile => "#c9a227",
            Activity::Publish => "#c98127",
            Activity::Index => "#a0a0a8",
            Activity::Compile => "#d4693f",
            Activity::Package => "#c4508a",
            Activity::Run => "#9b59b6",
            Activity::Test => "#4f9ad4",
            Activity::Format => "#7f8c8d",
            Activity::Lint => "#95a5a6",
        }
    }

    const fn outcome_class(outcome: Outcome) -> &'static str {
        match outcome {
            Outcome::Completed => "ok",
            Outcome::Fresh => "fresh",
            Outcome::Skipped => "skipped",
            Outcome::Failed => "failed",
            Outcome::Abandoned => "abandoned",
        }
    }

    /// HTML-escaped text.
    ///
    /// A subject is a URL, a jar name, or a host path, so `&` and `<` reach here routinely. The
    /// quote forms matter because subjects land inside `title="…"` attributes.
    fn escaped(text: &str) -> String {
        let mut escaped = String::with_capacity(text.len());
        for character in text.chars() {
            match character {
                '&' => escaped.push_str("&amp;"),
                '<' => escaped.push_str("&lt;"),
                '>' => escaped.push_str("&gt;"),
                '"' => escaped.push_str("&quot;"),
                '\'' => escaped.push_str("&#39;"),
                other => escaped.push(other),
            }
        }
        escaped
    }

    /// Append formatted text. Writing to a `String` cannot fail, and saying so once here beats an
    /// `expect` at every call site.
    fn put(&mut self, args: Arguments<'_>) {
        let _ = self.out.write_fmt(args);
    }
}

/// The page's whole stylesheet. Inline, because the report has to open offline from a `file://`
/// URL — which is exactly the machine that wanted to know why its build was slow.
const STYLE: &str = "\
:root{color-scheme:light dark;--bg:#fbfbfd;--fg:#1c1c22;--dim:#6b6b76;--line:#e2e2e8;--card:#fff}\
@media (prefers-color-scheme:dark){:root{--bg:#16161a;--fg:#e8e8ee;--dim:#9a9aa6;--line:#2c2c34;--card:#1e1e24}}\
*{box-sizing:border-box}\
body{margin:0;padding:2rem 1.5rem 4rem;background:var(--bg);color:var(--fg);\
font:14px/1.5 ui-sans-serif,-apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif}\
h1{font-size:1.25rem;margin:0}h2{font-size:.85rem;text-transform:uppercase;letter-spacing:.08em;color:var(--dim);margin:2rem 0 .75rem}\
header{border-bottom:1px solid var(--line);padding-bottom:1rem}\
.cmd{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;color:var(--dim);margin:.35rem 0 0;word-break:break-all}\
.cards{display:flex;flex-wrap:wrap;gap:.75rem;margin-top:1.5rem}\
.card{background:var(--card);border:1px solid var(--line);border-radius:8px;padding:.6rem .9rem;min-width:6rem;display:flex;flex-direction:column}\
.card .v{font-size:1.15rem;font-variant-numeric:tabular-nums}.card .k{font-size:.72rem;color:var(--dim);text-transform:uppercase;letter-spacing:.06em}\
table{width:100%;border-collapse:collapse}th{text-align:left;font-size:.72rem;color:var(--dim);text-transform:uppercase;letter-spacing:.06em;font-weight:600}\
th,td{padding:.3rem .5rem;border-bottom:1px solid var(--line)}\
td.num{text-align:right;font-variant-numeric:tabular-nums;white-space:nowrap}\
td.track{width:40%}td.track span{display:block;height:.5rem;border-radius:3px;min-width:2px}\
.dot{display:inline-block;width:.6rem;height:.6rem;border-radius:2px;margin-right:.5rem;vertical-align:-1px}\
.gantt{border:1px solid var(--line);border-radius:8px;overflow-x:auto;background:var(--card)}\
.row{display:grid;grid-template-columns:minmax(9rem,22%) 1fr 4.5rem;align-items:center;gap:.5rem;padding:.15rem .6rem;border-bottom:1px solid var(--line)}\
.row:last-child{border-bottom:0}\
.label{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:.78rem;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}\
.lane{position:relative;height:.9rem}\
.bar{position:absolute;top:.15rem;height:.6rem;border-radius:3px;min-width:2px}\
.bar.fresh{opacity:.35}.bar.skipped{opacity:.25}\
.bar.failed{outline:2px solid #d64545;outline-offset:1px}\
.bar.abandoned{outline:1px dashed var(--dim);outline-offset:1px}\
.time{text-align:right;font-size:.75rem;color:var(--dim);font-variant-numeric:tabular-nums}\
.plot{width:100%;height:160px;background:var(--card);border:1px solid var(--line);border-radius:8px}\
.plot polyline{fill:none;stroke:#3f9dd4;stroke-width:2;vector-effect:non-scaling-stroke}\
.axis{display:flex;justify-content:space-between;color:var(--dim);font-size:.75rem;margin:.35rem .1rem 0}\
.empty{color:var(--dim);padding:1rem}\
";
