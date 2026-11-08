#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]
use jetsearch::{
    index::{self, Found, Index, Opts},
    storage,
    worker::SearchWorker,
};
use slint::{ModelRc, VecModel};
use std::{
    cell::RefCell,
    sync::atomic::{AtomicU64, Ordering},
    fs,
    path::PathBuf,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

slint::include_modules!();

type Shared = Arc<RwLock<Arc<Index>>>;

/// Bumped per rescan; a finishing scan from an older generation is discarded.
static SCAN_GEN: AtomicU64 = AtomicU64::new(0);
/// Set when a DB write failed: next save does a full replace instead of a diff.
static DB_DIRTY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn db_path() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_DATA_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("JetSearch").join("index.sqlite"))
}

thread_local! {
    // full paths of the rows on screen; only touched on the UI thread
    static PATHS: RefCell<Vec<String>> = RefCell::default();
}

fn opts(ui: &AppWindow) -> Opts {
    Opts { case: ui.get_match_case(), path: ui.get_search_path(), hidden: ui.get_show_hidden(), system: ui.get_show_system() }
}

thread_local! {
    // status line shown when the query box is empty
    static IDLE: RefCell<String> = RefCell::default();
}

/// Runs on the UI thread. Applies one finished search to the window.
fn show(ui: &AppWindow, f: Found, took: Duration, n: usize) {
    if ui.get_query().trim().is_empty() || n == 0 {
        PATHS.with(|p| p.borrow_mut().clear());
        ui.set_rows(ModelRc::default());
        let idle = IDLE.with(|i| i.borrow().clone());
        ui.set_status(if n == 0 { "Indexing…".into() } else if idle.is_empty() { format!("{n} items indexed") } else { idle }.into());
        return;
    }
    PATHS.with(|p| *p.borrow_mut() = f.hits.iter().map(|h| h.full()).collect());
    let rows: Vec<FileRow> =
        f.hits.iter().map(|h| FileRow { name: h.name.clone().into(), dir: h.dir.clone().into(), is_dir: h.is_dir }).collect();
    ui.set_rows(ModelRc::new(VecModel::from(rows)));
    let shown = f.hits.len();
    let ms = took.as_millis();
    ui.set_status(if f.total > shown { format!("Showing {shown} of {} · {ms} ms", f.total) } else { format!("{} results · {ms} ms", f.total) }.into());
}

/// Queue a search for the current UI state; never blocks.
fn request(ui: &AppWindow, w: &SearchWorker) {
    w.submit(ui.get_query().to_string(), opts(ui));
}

fn rescan(ui: &AppWindow, shared: &Shared, worker: &Arc<SearchWorker>) {
    let gen = SCAN_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    // keep serving the previous (persisted) index until the new one is ready
    ui.set_status(format!("{} items · rescanning…", shared.read().unwrap().len()).into());
    let (weak, shared, worker) = (ui.as_weak(), shared.clone(), worker.clone());
    // previous snapshot, needed to diff against; both indexes coexist in RAM until the scan ends
    let old = shared.read().unwrap().clone();
    std::thread::spawn(move || {
        let t = Instant::now();
        let idx = Arc::new(Index::build(&index::roots()));
        if SCAN_GEN.load(Ordering::SeqCst) != gen {
            return; // superseded by a newer rescan
        }
        *shared.write().unwrap() = idx.clone();
        let mut err = None;
        let mut change = None;
        if let Some(p) = db_path() {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
            let r = storage::open(&p).and_then(|mut c| {
                if old.len() == 0 || DB_DIRTY.load(Ordering::SeqCst) {
                    storage::save(&mut c, &idx, now)
                } else {
                    let d = index::diff(&old, &idx);
                    change = Some(d.summary());
                    storage::apply(&mut c, &d, now)
                }
            });
            DB_DIRTY.store(r.is_err(), Ordering::SeqCst);
            err = r.err().map(|e| e.to_string());
        }
        drop(old);
        let secs = t.elapsed().as_secs_f32();
        weak.upgrade_in_event_loop(move |ui| {
            let msg = match err {
                Some(e) => format!("{} items indexed in {secs:.1}s · save failed: {e}", idx.len()),
                None => match change {
                    Some(c) => format!("{} items · scan {secs:.1}s · changes {c}", idx.len()),
                    None => format!("{} items indexed in {secs:.1}s", idx.len()),
                },
            };
            IDLE.with(|i| *i.borrow_mut() = msg);
            request(&ui, &worker); // re-run the current query against the new index
        })
        .ok();
    });
}

fn open(path: &str, reveal: bool) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut c = std::process::Command::new("explorer.exe");
        if reveal {
            c.raw_arg(format!("/select,\"{path}\""));
        } else {
            c.arg(path);
        }
        let _ = c.spawn();
    }
    #[cfg(not(windows))]
    {
        let target = match std::path::Path::new(path).parent() {
            Some(p) if reveal => p.to_string_lossy().into_owned(),
            _ => path.to_string(),
        };
        let _ = std::process::Command::new("xdg-open").arg(target).spawn();
    }
}

// settings live in a tiny key=value file, no serde needed
fn cfg_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("jetsearch.cfg"))
}

fn load(ui: &AppWindow) {
    let Some(text) = cfg_path().and_then(|p| fs::read_to_string(p).ok()) else { return };
    for (k, v) in text.lines().filter_map(|l| l.split_once('=')) {
        let on = v == "1";
        match k {
            "case" => ui.set_match_case(on),
            "path" => ui.set_search_path(on),
            "hidden" => ui.set_show_hidden(on),
            "system" => ui.set_show_system(on),
            _ => {}
        }
    }
}

fn save(ui: &AppWindow) {
    let Some(p) = cfg_path() else { return };
    let b = |x: bool| x as u8;
    let text = format!(
        "case={}\npath={}\nhidden={}\nsystem={}\n",
        b(ui.get_match_case()),
        b(ui.get_search_path()),
        b(ui.get_show_hidden()),
        b(ui.get_show_system())
    );
    let _ = fs::write(p, text);
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    load(&ui);
    let shared = Shared::default();
    // instant start from the persisted index; a background rescan then refreshes it
    if let Some(ix) = db_path().filter(|p| p.exists()).and_then(|p| storage::open(&p).ok()).and_then(|c| storage::load(&c).ok().flatten()) {
        *shared.write().unwrap() = Arc::new(ix);
    }

    let weak = ui.as_weak();
    let latest: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    let (lw, sh) = (latest.clone(), shared.clone());
    let worker = Arc::new(SearchWorker::new(shared.clone(), latest, move |gen, found, took| {
        let (lw, sh) = (lw.clone(), sh.clone());
        weak.upgrade_in_event_loop(move |ui| {
            // drop results that were superseded while waiting for the UI thread
            if lw.load(Ordering::SeqCst) == gen {
                let n = sh.read().unwrap().len();
                show(&ui, found, took, n);
            }
        })
        .ok();
    }));

    let (w, k) = (ui.as_weak(), worker.clone());
    ui.on_query_changed(move || request(&w.unwrap(), &k));

    let (w, k) = (ui.as_weak(), worker.clone());
    ui.on_settings_changed(move || {
        let ui = w.unwrap();
        save(&ui);
        request(&ui, &k);
    });

    let (w, s, k) = (ui.as_weak(), shared.clone(), worker.clone());
    ui.on_rescan(move || rescan(&w.unwrap(), &s, &k));

    ui.on_open_item(|i| {
        if let Some(p) = PATHS.with(|p| p.borrow().get(i as usize).cloned()) {
            open(&p, false);
        }
    });
    ui.on_reveal_item(|i| {
        if let Some(p) = PATHS.with(|p| p.borrow().get(i as usize).cloned()) {
            open(&p, true);
        }
    });

    request(&ui, &worker);
    rescan(&ui, &shared, &worker);
    let r = ui.run();
    SCAN_GEN.fetch_add(1, Ordering::SeqCst);
    r
}