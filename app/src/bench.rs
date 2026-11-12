//! `excavate --bench [--bench-out file.json]`: measures the real app on this PC, end to end, and writes a report.
//!
//! - popup: hotkey-equivalent `show()` until its first frame is on screen, cold (the window's first creation) and
//!   warm (ten hide/show cycles);
//! - typing: queries typed a character at a time (80 ms apart, a fast typist); per keystroke, the time until the hot
//!   tier's frame is drawn; per query, the time from its last keystroke until Everything's results are drawn;
//! - memory once idle and hidden;
//! - with `--bench-switch`, Excavate's own index switched while running, as Settings does, and back.

use crate::app::App;
use crate::with_app;
use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

thread_local! {
    static RUNNING: Cell<bool> = const { Cell::new(false) };
    static FRAME: RefCell<(u64, Vec<Waker>)> = const { RefCell::new((0, Vec::new())) };
    static SHOWN: RefCell<Vec<f64>> = const { RefCell::new(Vec::new()) };
}

pub fn running() -> bool {
    RUNNING.with(Cell::get)
}

/// A frame was drawn.
pub fn frame() {
    let wakers = FRAME.with(|frame| {
        let mut frame = frame.borrow_mut();
        frame.0 += 1;
        std::mem::take(&mut frame.1)
    });
    for waker in wakers {
        waker.wake();
    }
}

/// The popup's first frame after `show()`.
pub fn shown(elapsed: Duration) {
    if running() {
        SHOWN.with(|shown| shown.borrow_mut().push(elapsed.as_secs_f64() * 1000.0));
    }
}

struct NextFrame {
    after: u64,
}

impl Future for NextFrame {
    type Output = ();
    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        FRAME.with(|frame| {
            let mut frame = frame.borrow_mut();
            if frame.0 > self.after {
                Poll::Ready(())
            } else {
                frame.1.push(context.waker().clone());
                Poll::Pending
            }
        })
    }
}

fn next_frame() -> NextFrame {
    NextFrame { after: FRAME.with(|frame| frame.borrow().0) }
}

struct Sleep {
    done: Rc<Cell<bool>>,
    duration: Duration,
    started: bool,
}

impl Future for Sleep {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        if self.done.get() {
            return Poll::Ready(());
        }
        if !self.started {
            self.started = true;
            let (done, waker) = (self.done.clone(), context.waker().clone());
            slint::Timer::single_shot(self.duration, move || {
                done.set(true);
                waker.wake();
            });
        }
        Poll::Pending
    }
}

fn sleep(milliseconds: u64) -> Sleep {
    Sleep { done: Rc::new(Cell::new(false)), duration: Duration::from_millis(milliseconds), started: false }
}

const QUERIES: &[&str] = &["code", "budget", "screenshot", "explorer.exe", "q3 report", "readme", "vsc", "zzqqxx", "ext:pdf dm:thisyear"];

fn stats(values: &[f64]) -> serde_json::Value {
    if values.is_empty() {
        return serde_json::Value::Null;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let at = |q: f64| sorted[((sorted.len() - 1) as f64 * q).round() as usize];
    serde_json::json!({ "n": sorted.len(), "median": at(0.5), "p90": at(0.9), "max": at(1.0), "min": at(0.0) })
}

/// A hash of result paths, compared without case and order within ties ignored: names never leave the process.
fn top_hash(paths: &[String]) -> String {
    use std::hash::{Hash, Hasher};
    let mut sorted: Vec<String> = paths.iter().map(|path| path.to_lowercase()).collect();
    sorted.sort();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    sorted.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

struct Typed {
    keystrokes: Vec<f64>,
    main_drawn: f64,
    refined_drawn: f64,
    summary: serde_json::Value,
}

/// Types `query` a character at a time (80 ms apart, a fast typist), each keystroke timed to its frame, then waits for
/// the names' results to be drawn.
async fn type_query(query: &str) -> Typed {
    let characters: Vec<char> = query.chars().collect();
    let mut per_query = Vec::new();
    for end in 1..=characters.len() {
        let prefix: String = characters[..end].iter().collect();
        let typed = Instant::now();
        with_app(|app| {
            if let Some(ui) = app.ui.upgrade() {
                ui.set_query(prefix.clone().into());
            }
            app.edited(&prefix);
        });
        next_frame().await;
        per_query.push(typed.elapsed().as_secs_f64() * 1000.0);
        if end < characters.len() {
            sleep(80).await;
        }
    }
    let last = Instant::now();
    let mut waited = 0;
    let mut main_drawn = None;
    loop {
        let done = RefCell::new((false, false));
        with_app(|app| *done.borrow_mut() = (app.main_done(), app.everything_done()));
        let (main, all) = done.into_inner();
        if main && main_drawn.is_none() {
            next_frame().await;
            main_drawn = Some(last.elapsed().as_secs_f64() * 1000.0);
        }
        if all || waited > 5000 {
            break;
        }
        sleep(5).await;
        waited += 5;
    }
    next_frame().await;
    let refined_drawn = last.elapsed().as_secs_f64() * 1000.0;
    let main_drawn = main_drawn.unwrap_or(refined_drawn);
    let summary = RefCell::new(serde_json::Value::Null);
    with_app(|app| *summary.borrow_mut() = serde_json::json!({
        "query": query,
        "keystroke_frames_ms": per_query,
        "until_everything_drawn_ms": main_drawn,
        "until_refined_drawn_ms": refined_drawn,
        "everything_round_trip_ms": app.everything_ms(),
        "results": app.result_count(),
        "total": app.total(),
        // The first results, as a hash: the same with the own index on and off, or not (no names in the report).
        "top_hash": top_hash(&app.top(20)),
    }));
    Typed { keystrokes: per_query, main_drawn, refined_drawn, summary: summary.into_inner() }
}

/// What the search window's footer says (Everything's state, or the own index's).
fn banner() -> String {
    let text = RefCell::new(String::new());
    with_app(|app| {
        if let Some(ui) = app.ui.upgrade() {
            *text.borrow_mut() = ui.get_banner().to_string();
        }
    });
    text.into_inner()
}

pub fn run(app: &Rc<App>) {
    RUNNING.with(|running| running.set(true));
    let out = std::env::args().skip_while(|arg| arg != "--bench-out").nth(1)
        .unwrap_or_else(|| app.paths.local.join("bench.json").display().to_string());
    let renderer = std::env::var("EXCAVATE_RENDERER").unwrap_or_else(|_| app.settings.borrow().renderer.clone());
    let _ = slint::spawn_local(async move {
        // Let the hot tier build and Everything warm up, as after a sign-in.
        for _ in 0..100 {
            sleep(100).await;
            let ready = RefCell::new(false);
            with_app(|app| *ready.borrow_mut() = app.hot_len() > 0);
            if *ready.borrow() {
                break;
            }
        }
        // Excavate's own index, when it's on: searched once it's live (up to 10 minutes for a first build).
        let own_started = Instant::now();
        while crate::own_index::state().is_some_and(|state| state.phase != engine::names::Phase::Live) && own_started.elapsed() < Duration::from_secs(600) {
            sleep(200).await;
        }
        // (An index saved without dates made and opened lists the drives once more for them: measured once that's done.)
        sleep(500).await;
        while crate::own_index::state().is_some_and(|state| state.filling) && own_started.elapsed() < Duration::from_secs(900) {
            sleep(500).await;
        }
        let own = crate::own_index::state().map(|state| serde_json::json!({
            "phase": format!("{:?}", state.phase), "names": state.entries, "drives": state.volumes,
            "loaded_s": state.loaded_in.map(|d| d.as_secs_f64()), "built_s": state.built_in.map(|d| d.as_secs_f64()),
            "caught_up_s": state.caught_up_in.map(|d| d.as_secs_f64()), "refused_folders": state.refused, "watches": state.watches,
            "times_filled_s": state.times_filled_in.map(|d| d.as_secs_f64()),
        }));
        sleep(500).await;
        let (memory_before, _) = winsys::process_memory();

        with_app(|app| crate::popup::show(app));
        next_frame().await;
        sleep(300).await;
        for _ in 0..10 {
            with_app(|app| crate::popup::hide(app));
            sleep(150).await;
            with_app(|app| crate::popup::show(app));
            next_frame().await;
            sleep(100).await;
        }
        let shows = SHOWN.with(|shown| shown.borrow().clone());

        let mut keystrokes = Vec::new();
        let mut complete = Vec::new();
        let mut refined = Vec::new();
        let mut queries = Vec::new();
        for query in QUERIES {
            let typed = type_query(query).await;
            keystrokes.extend(typed.keystrokes.iter().copied());
            complete.push(typed.main_drawn);
            refined.push(typed.refined_drawn);
            queries.push(typed.summary);
            sleep(400).await;
        }
        let footer = banner();
        with_app(|app| crate::popup::hide(app));
        sleep(3000).await;
        let (working_set, private) = winsys::process_memory();

        // `--bench-switch`: the setting flipped while running, as Settings' switch does, and back: how soon each side
        // answers, what it answers, what the footer says, and the memory once switched (an index switched off is let go).
        let mut switches = Vec::new();
        if std::env::args().any(|arg| arg == "--bench-switch") {
            let initial = RefCell::new(false);
            with_app(|app| *initial.borrow_mut() = app.settings.borrow().own_index);
            let initial = initial.into_inner();
            with_app(|app| crate::popup::show(app));
            next_frame().await;
            for on in [!initial, initial] {
                let flipped = Instant::now();
                with_app(|app| {
                    app.settings.borrow_mut().own_index = on;
                    crate::own_index::apply(app);
                    app.set_everything_health(everything::Health::Unknown);
                    crate::popup::check_everything(app);
                });
                // Answering: the own index loaded (on), or Everything asked again (off).
                while flipped.elapsed() < Duration::from_secs(600) {
                    let ready = if on {
                        crate::own_index::state().is_some_and(|state| matches!(state.phase, engine::names::Phase::CatchingUp | engine::names::Phase::Live))
                    } else {
                        !everything::is_local()
                    };
                    if ready {
                        break;
                    }
                    sleep(20).await;
                }
                let answering_ms = flipped.elapsed().as_secs_f64() * 1000.0;
                with_app(|app| {
                    if let Some(ui) = app.ui.upgrade() {
                        ui.set_query("".into());
                    }
                    app.edited("");
                });
                sleep(300).await;
                let typed = type_query("readme").await;
                // Settled: the footer as it stays, and the memory once a switched-off index is gone.
                sleep(4000).await;
                let (_, private) = winsys::process_memory();
                let own = crate::own_index::state().map(|state| format!("{:?}", state.phase));
                switches.push(serde_json::json!({
                    "own_index": on,
                    "answered_by_own_index": everything::is_local(),
                    "own_phase": own,
                    "until_answering_ms": answering_ms,
                    "query": typed.summary,
                    "banner": banner(),
                    "private_mb": private as f64 / 1048576.0,
                }));
            }
        }

        with_app(|app| crate::popup::hide(app));
        let hot = RefCell::new((0usize, 0u128));
        with_app(|app| *hot.borrow_mut() = (app.hot_len(), app.hot_build_ms()));
        let (hot_len, hot_ms) = hot.into_inner();
        let report = serde_json::json!({
            "renderer": renderer,
            "own_index": own,
            "answered_by_own_index": everything::is_local(),
            "hot_tier": { "entries": hot_len, "build_ms": hot_ms },
            "popup_show_ms": { "cold": shows.first(), "warm": stats(shows.get(1..).unwrap_or(&[])) },
            "keystroke_to_frame_ms": stats(&keystrokes),
            "last_keystroke_to_everything_drawn_ms": stats(&complete),
            "last_keystroke_to_refinement_drawn_ms": stats(&refined),
            "memory_mb": {
                "working_set_idle": working_set as f64 / 1048576.0,
                "private_idle": private as f64 / 1048576.0,
                "working_set_before_first_show": memory_before as f64 / 1048576.0,
            },
            "queries": queries,
            "banner": footer,
            "switches": switches,
        });
        let _ = std::fs::write(&out, serde_json::to_string_pretty(&report).unwrap_or_default());
        crate::settings::log(&format!("bench written to {out}"));
        with_app(|app| crate::popup::quit(app));
    });
}
