//! The crate's own suite.
//!
//! Everything here runs with no terminal, no clock, and no host: the whole point of keeping the
//! ledger and its rendering portable is that CI's `wasm32-wasip1` cell can assert on both.

use alloc::{
    borrow::ToOwned,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use std::sync::Mutex;

use crate::{
    Activity, Event, Outcome, PackageRef, Progress, ReportMeta, Sink, Timeline, Unit, UnitId,
};

/// Every event, in order. The sink the suite asserts against.
#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<Event>>,
}

impl Recorder {
    fn events(&self) -> Vec<Event> {
        self.events
            .lock()
            .expect("recorder is never poisoned")
            .clone()
    }

    /// A `Progress` reporting into a fresh recorder, and the recorder.
    fn wired() -> (Progress, Arc<Self>) {
        let recorder = Arc::new(Self::default());
        (Progress::to(recorder.clone()), recorder)
    }
}

impl Sink for Recorder {
    fn emit(&self, event: &Event) {
        self.events
            .lock()
            .expect("recorder is never poisoned")
            .push(event.clone());
    }
}

#[test]
fn a_silent_handle_builds_no_event_and_no_subject() {
    let progress = Progress::SILENT;
    let task = progress.begin(Activity::Fetch, "client.jar");
    assert!(task.id().is_none());
    task.advance(10);
    task.finish(Outcome::Completed);
}

#[test]
fn a_unit_reports_start_progress_and_end_in_order() {
    let (progress, recorder) = Recorder::wired();
    let task = progress.begin_bounded(Activity::Fetch, "client.jar", 100);
    task.advance(40);
    task.advance(60);
    task.finish(Outcome::Completed);

    let events = recorder.events();
    assert_eq!(events.len(), 4, "{events:?}");
    let Event::Started { id, unit } = &events[0] else {
        panic!("first event is the start: {events:?}");
    };
    assert_eq!(unit.activity, Activity::Fetch);
    assert_eq!(unit.subject, "client.jar");
    assert_eq!(unit.total, Some(100));
    assert!(unit.package.is_none());
    assert_eq!(
        events[1],
        Event::Advanced {
            id: *id,
            done: 40,
            total: Some(100)
        }
    );
    // `advance` accumulates: a producer that counts by delta and one that counts absolutely both
    // arrive at the same place.
    assert_eq!(
        events[2],
        Event::Advanced {
            id: *id,
            done: 100,
            total: Some(100)
        }
    );
    assert_eq!(
        events[3],
        Event::Finished {
            id: *id,
            outcome: Outcome::Completed
        }
    );
}

#[test]
fn a_task_dropped_without_finishing_reports_abandoned() {
    let (progress, recorder) = Recorder::wired();
    drop(progress.begin(Activity::Remap, "server.jar"));
    let events = recorder.events();
    assert_eq!(
        events.last(),
        Some(&Event::Finished {
            id: UnitId::new(0),
            outcome: Outcome::Abandoned
        }),
        "{events:?}"
    );
}

#[test]
fn a_total_learned_late_reaches_the_consumer() {
    let (progress, recorder) = Recorder::wired();
    let task = progress.begin(Activity::Fetch, "server.jar");
    task.set_total(2048);
    task.set_done(512);
    task.finish(Outcome::Completed);

    let totals: Vec<Option<u64>> = recorder
        .events()
        .iter()
        .filter_map(|event| match event {
            Event::Advanced { total, .. } => Some(*total),
            _ => None,
        })
        .collect();
    assert_eq!(totals, [Some(2048), Some(2048)]);
}

#[test]
fn ids_are_dense_and_unique_across_clones_and_attributions() {
    let (progress, recorder) = Recorder::wired();
    let attributed = progress.for_package(PackageRef::new("hello", Some("0.1.0")));
    let first = progress.begin(Activity::Fetch, "a");
    let second = attributed.begin(Activity::Fetch, "b");
    let third = progress.begin(Activity::Fetch, "c");
    let ids: Vec<u64> = [&first, &second, &third]
        .iter()
        .map(|task| task.id().expect("live task has an id").get())
        .collect();
    assert_eq!(ids, [0, 1, 2]);
    drop((first, second, third));
    assert_eq!(recorder.events().len(), 6);
}

#[test]
fn attribution_rides_the_handle_rather_than_each_call() {
    let (progress, recorder) = Recorder::wired();
    let attributed = progress.for_package(PackageRef::new("hello", Some("0.1.0")));
    attributed.record(Activity::Compile, "", Outcome::Completed);
    // The unattributed handle it was derived from keeps reporting unattributed work.
    progress.record(Activity::Fetch, "mappings.txt", Outcome::Fresh);

    let units: Vec<Unit> = recorder
        .events()
        .iter()
        .filter_map(|event| match event {
            Event::Started { unit, .. } => Some(unit.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(units[0].describe(), "hello v0.1.0");
    assert_eq!(units[1].package, None);
    assert_eq!(units[1].describe(), "mappings.txt");
}

#[test]
fn a_package_and_a_subject_read_as_cargo_spells_them() {
    let unit = Unit {
        package: Some(PackageRef::new("hello", Some("0.1.0"))),
        activity: Activity::Remap,
        subject: "server.jar".to_owned(),
        total: None,
    };
    assert_eq!(unit.describe(), "hello v0.1.0 (server.jar)");
    assert_eq!(
        PackageRef::unversioned("shared").to_string(),
        "shared",
        "a package with no declared version is named without one"
    );
}

/// The suite's fixtures, as associated functions rather than free ones — this file lives in `src`,
/// where the workspace's `no-free-functions` rule applies.
struct Fixture;

impl Fixture {
    /// A ledger fed one span per `(activity, start, end)`, so the timing tests read as a table.
    fn ledger(spans: &[(Activity, &str, u64, u64, Outcome)]) -> Timeline {
        let mut timeline = Timeline::new();
        for (index, (activity, subject, start, end, outcome)) in spans.iter().enumerate() {
            let id = UnitId::new(index as u64);
            timeline.record(
                &Event::Started {
                    id,
                    unit: Unit {
                        package: None,
                        activity: *activity,
                        subject: (*subject).to_owned(),
                        total: None,
                    },
                },
                *start,
            );
            timeline.record(
                &Event::Finished {
                    id,
                    outcome: *outcome,
                },
                *end,
            );
        }
        timeline
    }

    fn meta() -> ReportMeta {
        ReportMeta {
            command: "jals build --features <client>".to_owned(),
            project: Some("hello".to_owned()),
            total_micros: 3_000_000,
        }
    }
}

#[test]
fn a_ledger_pairs_each_start_with_its_own_end() {
    let timeline = Fixture::ledger(&[
        (Activity::Fetch, "a.jar", 0, 1_000_000, Outcome::Completed),
        (
            Activity::Remap,
            "a.jar",
            500_000,
            2_500_000,
            Outcome::Completed,
        ),
    ]);
    let spans = timeline.spans();
    assert_eq!(spans.len(), 2);
    assert_eq!(spans[0].duration_micros(), 1_000_000);
    assert_eq!(spans[1].duration_micros(), 2_000_000);
}

#[test]
fn an_event_for_a_unit_the_ledger_never_saw_start_is_dropped() {
    let mut timeline = Timeline::new();
    timeline.record(
        &Event::Finished {
            id: UnitId::new(7),
            outcome: Outcome::Completed,
        },
        10,
    );
    timeline.record(
        &Event::Advanced {
            id: UnitId::new(7),
            done: 1,
            total: None,
        },
        11,
    );
    assert!(
        timeline.spans().is_empty(),
        "a partial stream is a partial report, not a fabricated one"
    );
}

#[test]
fn activity_totals_are_ordered_by_time_and_count_their_units() {
    let timeline = Fixture::ledger(&[
        (Activity::Fetch, "a", 0, 1_000_000, Outcome::Completed),
        (Activity::Fetch, "b", 0, 2_000_000, Outcome::Completed),
        (Activity::Remap, "c", 0, 500_000, Outcome::Completed),
    ]);
    let totals = timeline.by_activity();
    assert_eq!(totals[0], (Activity::Fetch, 3_000_000, 2));
    assert_eq!(totals[1], (Activity::Remap, 500_000, 1));
}

#[test]
fn concurrency_peaks_where_the_spans_overlap() {
    let timeline = Fixture::ledger(&[
        (Activity::Fetch, "a", 0, 100, Outcome::Completed),
        (Activity::Fetch, "b", 40, 100, Outcome::Completed),
        (Activity::Fetch, "c", 40, 60, Outcome::Completed),
    ]);
    let samples = timeline.concurrency(100, 10);
    assert_eq!(samples.len(), 10);
    assert_eq!(samples[0], 1, "only the first unit has started");
    assert_eq!(samples[7], 2, "the third has ended, the other two have not");
    assert_eq!(
        samples.iter().copied().max(),
        Some(3),
        "all three overlap somewhere: {samples:?}"
    );
}

#[test]
fn the_html_report_is_self_contained() {
    let timeline = Fixture::ledger(&[
        (
            Activity::Fetch,
            "client.jar",
            0,
            1_000_000,
            Outcome::Completed,
        ),
        (
            Activity::Remap,
            "client.jar",
            1_000_000,
            3_000_000,
            Outcome::Fresh,
        ),
    ]);
    let html = timeline.html(&Fixture::meta());
    assert!(html.starts_with("<!doctype html>"));
    assert!(html.trim_end().ends_with("</html>"));
    for fetched in ["http://", "https://", "//cdn", "<script", "src="] {
        assert!(
            !html.contains(fetched),
            "the report must not reach the network or run script: found {fetched}"
        );
    }
}

#[test]
fn the_html_report_escapes_what_a_subject_can_contain() {
    let timeline = Fixture::ledger(&[(
        Activity::Fetch,
        "https://example.invalid/a.jar?x=1&y=\"<b>\"",
        0,
        1_000,
        Outcome::Completed,
    )]);
    let html = timeline.html(&Fixture::meta());
    assert!(html.contains("x=1&amp;y=&quot;&lt;b&gt;&quot;"), "{html}");
    assert!(
        !html.contains("<b>"),
        "a subject must never reach the page as markup"
    );
}

#[test]
fn the_html_report_places_a_span_by_its_share_of_the_run() {
    let timeline = Fixture::ledger(&[(
        Activity::Compile,
        "hello",
        1_500_000,
        3_000_000,
        Outcome::Completed,
    )]);
    let html = timeline.html(&Fixture::meta());
    assert!(html.contains("left:50.00%"), "{html}");
    assert!(html.contains("width:50.00%"), "{html}");
}

#[test]
fn an_empty_run_still_renders_a_page() {
    let html = Timeline::new().html(&ReportMeta {
        command: "jals build".to_owned(),
        project: None,
        total_micros: 0,
    });
    assert!(html.starts_with("<!doctype html>"));
    assert!(html.contains("nothing reported any work"));
}

#[test]
fn the_json_report_carries_the_run_and_every_span() {
    let timeline =
        Fixture::ledger(&[(Activity::Fetch, "client.jar", 0, 1_000_000, Outcome::Fresh)]);
    let json: String = timeline.json(&Fixture::meta());
    assert!(
        json.contains("\"command\":\"jals build --features <client>\""),
        "{json}"
    );
    assert!(json.contains("\"project\":\"hello\""), "{json}");
    assert!(json.contains("\"activity\":\"fetch\""), "{json}");
    assert!(json.contains("\"outcome\":\"fresh\""), "{json}");
    assert!(json.contains("\"start_micros\":0"), "{json}");
    assert!(json.contains("\"end_micros\":1000000"), "{json}");
}

#[test]
fn an_event_serializes_under_a_tag_a_reader_can_switch_on() {
    let event = Event::Started {
        id: UnitId::new(3),
        unit: Unit {
            package: Some(PackageRef::unversioned("hello")),
            activity: Activity::Decompile,
            subject: "server.jar".to_owned(),
            total: Some(12),
        },
    };
    let json = serde_json::to_string(&event).expect("an event serializes");
    assert!(json.contains("\"event\":\"started\""), "{json}");
    assert!(json.contains("\"activity\":\"decompile\""), "{json}");
    assert_eq!(event.id(), UnitId::new(3));
}

#[test]
fn a_unit_ends_exactly_once_however_many_endings_it_is_given() {
    let (progress, recorder) = Recorder::wired();
    let task = progress.begin(Activity::Remap, "server.jar");
    // A step deep inside the work answers from a memo; the caller that started the unit still
    // finishes it, and the drop behind that would say something a third time.
    task.fresh();
    task.finish(Outcome::Completed);

    let endings: Vec<Outcome> = recorder
        .events()
        .iter()
        .filter_map(|event| match event {
            Event::Finished { outcome, .. } => Some(*outcome),
            _ => None,
        })
        .collect();
    assert_eq!(endings, [Outcome::Fresh], "the first ending is the one");
}

#[test]
fn a_ticker_counts_into_the_unit_that_handed_it_out() {
    let (progress, recorder) = Recorder::wired();
    let task = progress.begin_bounded(Activity::Remap, "server.jar", 3);
    let ticker = task.ticker();
    let cloned = ticker.clone();
    ticker.tick();
    // A clone counts into the same place: that is what a fan-out worker holds.
    cloned.tick();
    task.advance(1);
    task.finish(Outcome::Completed);

    let counts: Vec<u64> = recorder
        .events()
        .iter()
        .filter_map(|event| match event {
            Event::Advanced { done, .. } => Some(*done),
            _ => None,
        })
        .collect();
    assert_eq!(
        counts,
        [1, 2, 3],
        "a ticker and its task count into one place"
    );
}

#[test]
fn only_work_that_actually_ran_counts_as_having_run() {
    assert!(Outcome::Completed.ran());
    assert!(Outcome::Failed.ran());
    assert!(!Outcome::Fresh.ran());
    assert!(!Outcome::Skipped.ran());
    assert!(!Outcome::Abandoned.ran());
}
