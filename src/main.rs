#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]
mod index;

use index::{Index, Opts};
use slint::{ModelRc, VecModel};
use std::{
    cell::RefCell,
    fs,
    path::PathBuf,
    sync::{Arc, RwLock},
    time::Instant,
};

slint::include_modules!();

type Shared = Arc<RwLock<Arc<Index>>>;

thread_local! {
    // full paths of the rows on screen; only touched on the UI thread
    static PATHS: RefCell<Vec<String>> = RefCell::default();
}

fn opts(ui: &AppWindow) -> Opts {
    Opts { case: ui.get_match_case(), path: ui.get_search_path(), hidden: ui.get_show_hidden(), system: ui.get_show_system() }
}

fn refresh(ui: &AppWindow, idx: &Index) {
    let q = ui.get_query();
    if q.trim().is_empty() || idx.len() == 0 {
        PATHS.with(|p| p.borrow_mut().clear());
        ui.set_rows(ModelRc::default());
        let n = idx.len();
        ui.set_status(if n == 0 { "Indexing…".into() } else { format!("{n} items indexed").into() });
        return;
    }
    let t = Instant::now();
    let f = idx.search(&q, &opts(ui));
    PATHS.with(|p| *p.borrow_mut() = f.hits.iter().map(|h| h.full()).collect());
    let rows: Vec<FileRow> =
        f.hits.iter().map(|h| FileRow { name: h.name.clone().into(), dir: h.dir.clone().into(), is_dir: h.is_dir }).collect();
    ui.set_rows(ModelRc::new(VecModel::from(rows)));
    let shown = f.hits.len();
    let ms = t.elapsed().as_millis();
    ui.set_status(if f.total > shown { format!("Showing {shown} of {} · {ms} ms", f.total) } else { format!("{} results · {ms} ms", f.total) }.into());
}

fn rescan(ui: &AppWindow, shared: &Shared) {
    // drop the old index first so two indexes never sit in RAM at once
    *shared.write().unwrap() = Arc::default();
    refresh(ui, &Index::default());
    let (weak, shared) = (ui.as_weak(), shared.clone());
    std::thread::spawn(move || {
        let t = Instant::now();
        let idx = Arc::new(Index::build(&index::roots()));
        *shared.write().unwrap() = idx.clone();
        let secs = t.elapsed().as_secs_f32();
        weak.upgrade_in_event_loop(move |ui| {
            refresh(&ui, &idx);
            if ui.get_query().trim().is_empty() {
                ui.set_status(format!("{} items indexed in {secs:.1}s", idx.len()).into());
            }
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

    let (w, s) = (ui.as_weak(), shared.clone());
    ui.on_query_changed(move || refresh(&w.unwrap(), &s.read().unwrap()));

    let (w, s) = (ui.as_weak(), shared.clone());
    ui.on_settings_changed(move || {
        let ui = w.unwrap();
        save(&ui);
        refresh(&ui, &s.read().unwrap());
    });

    let (w, s) = (ui.as_weak(), shared.clone());
    ui.on_rescan(move || rescan(&w.unwrap(), &s));

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

    rescan(&ui, &shared);
    ui.run()
}
