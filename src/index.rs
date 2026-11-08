//! Compact in-memory file index: one string arena for all names, 12-byte entries,
//! one path string per directory. Searching is a parallel scan over the entries.
use rayon::prelude::*;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
    sync::Mutex,
};

const DIR: u8 = 1;
const HIDDEN: u8 = 2;
const SYSTEM: u8 = 4;
const LIMIT: usize = 200;

struct Entry {
    off: u32,
    dir: u32,
    mtime: u32, // unix secs, 0 = unknown
    size: u64,
    len: u16,
    flags: u8,
}

#[derive(Default)]
pub struct Index {
    dirs: Vec<String>,
    names: String,
    entries: Vec<Entry>,
}

struct Chunk {
    dir: String,
    names: String,
    meta: Vec<(u16, u8, u64, u32)>,
}

#[derive(Clone, Copy)]
pub struct Opts {
    pub case: bool,
    pub path: bool,
    pub hidden: bool,
    pub system: bool,
}

pub struct Hit {
    pub name: String,
    pub dir: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: u32,
}

impl Hit {
    pub fn full(&self) -> String {
        Path::new(&self.dir).join(&self.name).to_string_lossy().into_owned()
    }
}

pub struct Row {
    pub dir: String,
    pub name: String,
    pub flags: u8,
    pub size: u64,
    pub mtime: u32,
}

/// Difference between two index snapshots, keyed by (dir path, name).
#[derive(Default)]
pub struct Diff {
    pub upsert: Vec<Row>,
    pub removed: Vec<(String, String)>,
    pub gone_dirs: Vec<String>,
    pub added: usize,
    pub changed: usize,
    pub deleted: usize,
}

impl Diff {
    pub fn is_empty(&self) -> bool {
        self.upsert.is_empty() && self.deleted == 0 && self.gone_dirs.is_empty()
    }
    pub fn summary(&self) -> String {
        format!("+{} ~{} -{}", self.added, self.changed, self.deleted)
    }
}

pub fn diff(old: &Index, new: &Index) -> Diff {
    let old_ids: HashMap<&str, u32> = old.dirs.iter().enumerate().map(|(i, d)| (d.as_str(), i as u32)).collect();
    let new_dirs: HashSet<&str> = new.dirs.iter().map(String::as_str).collect();
    let mut om: HashMap<(u32, &str), (u8, u64, u32)> =
        old.entries.iter().map(|e| ((e.dir, old.name(e)), (e.flags, e.size, e.mtime))).collect();
    let mut d = Diff::default();
    for e in &new.entries {
        let dir = new.dirs[e.dir as usize].as_str();
        let name = new.name(e);
        let prev = old_ids.get(dir).and_then(|&od| om.remove(&(od, name)));
        let cur = (e.flags, e.size, e.mtime);
        match prev {
            Some(m) if m == cur => continue,
            Some(_) => d.changed += 1,
            None => d.added += 1,
        }
        d.upsert.push(Row { dir: dir.into(), name: name.into(), flags: e.flags, size: e.size, mtime: e.mtime });
    }
    d.deleted = om.len();
    for ((od, name), _) in om {
        let dir = old.dirs[od as usize].as_str();
        if new_dirs.contains(dir) {
            d.removed.push((dir.into(), name.into()));
        }
    }
    d.gone_dirs = old.dirs.iter().filter(|p| !new_dirs.contains(p.as_str())).cloned().collect();
    d
}

#[derive(Default)]
pub struct Found {
    pub hits: Vec<Hit>,
    pub total: usize,
}

struct Query {
    inc: Vec<Vec<u8>>,
    exc: Vec<Vec<u8>>,
    ext: Vec<Vec<u8>>,
}

impl Query {
    /// `a b` = AND, `-a` = exclude, `ext:pdf,docx` = extension filter.
    fn parse(s: &str, case: bool) -> Self {
        let fold = |t: &str| if case { t.as_bytes().to_vec() } else { t.to_ascii_lowercase().into_bytes() };
        let mut q = Query { inc: vec![], exc: vec![], ext: vec![] };
        for t in s.split_whitespace() {
            if let Some(e) = t.strip_prefix("ext:") {
                let exts = e.split(',').map(|x| x.trim_start_matches('.').to_ascii_lowercase().into_bytes());
                q.ext.extend(exts.filter(|x| !x.is_empty()));
            } else if let Some(x) = t.strip_prefix('-').filter(|x| !x.is_empty()) {
                q.exc.push(fold(x));
            } else {
                q.inc.push(fold(t));
            }
        }
        q
    }
}

fn find(h: &[u8], n: &[u8], case: bool) -> Option<usize> {
    if n.is_empty() {
        return None;
    }
    h.windows(n.len()).position(|w| if case { w == n } else { w.eq_ignore_ascii_case(n) })
}

/// Typo-tolerant fallback: all needle bytes appear in order.
fn subseq(h: &[u8], n: &[u8], case: bool) -> bool {
    let mut it = h.iter();
    n.iter().all(|c| it.any(|x| if case { x == c } else { x.eq_ignore_ascii_case(c) }))
}

fn ext_of(name: &[u8]) -> &[u8] {
    name.iter().rposition(|&b| b == b'.').map_or(&[], |d| &name[d + 1..])
}

#[cfg(windows)]
fn attr_flags(m: &fs::Metadata, _: &str) -> u8 {
    use std::os::windows::fs::MetadataExt;
    let a = m.file_attributes();
    (if a & 0x2 != 0 { HIDDEN } else { 0 }) | (if a & 0x4 != 0 { SYSTEM } else { 0 })
}

#[cfg(not(windows))]
fn attr_flags(_: &fs::Metadata, name: &str) -> u8 {
    if name.starts_with('.') { HIDDEN } else { 0 }
}

fn mtime_of(m: &fs::Metadata) -> u32 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs().min(u32::MAX as u64) as u32)
}

/// Walks one directory, then its subdirectories in parallel.
/// `inherit` carries hidden/system down so the contents of a hidden folder are hidden too.
fn scan(dir: &Path, inherit: u8, out: &Mutex<Vec<Chunk>>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut c = Chunk { dir: dir.to_string_lossy().into_owned(), names: String::new(), meta: vec![] };
    let mut subs = vec![];
    for e in rd.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        let name = e.file_name();
        let name = name.to_string_lossy();
        // one metadata call: free on Windows (comes with the directory listing)
        let md = e.metadata().ok();
        let mut flags = md.as_ref().map_or(0, |m| attr_flags(m, &name)) | inherit;
        let (size, mtime) = md.as_ref().map_or((0, 0), |m| (if ft.is_dir() { 0 } else { m.len() }, mtime_of(m)));
        // file_type() does not follow symlinks/junctions, so there are no directory loops
        if ft.is_dir() {
            subs.push((e.path(), flags & (HIDDEN | SYSTEM)));
            flags |= DIR;
        }
        c.meta.push((name.len() as u16, flags, size, mtime));
        c.names.push_str(&name);
    }
    out.lock().unwrap().push(c);
    subs.par_iter().for_each(|(d, f)| scan(d, *f, out));
}

#[cfg(windows)]
pub fn roots() -> Vec<String> {
    extern "system" {
        fn GetDriveTypeW(root: *const u16) -> u32;
    }
    ('A'..='Z')
        .map(|c| format!("{c}:\\"))
        // 3 = DRIVE_FIXED: skips network, USB and optical drives
        .filter(|p| {
            let w: Vec<u16> = p.encode_utf16().chain(Some(0)).collect();
            unsafe { GetDriveTypeW(w.as_ptr()) == 3 }
        })
        .collect()
}

#[cfg(not(windows))]
pub fn roots() -> Vec<String> {
    vec!["/".into()]
}

impl Index {
    /// (dir id, name, flags) for every entry, for persistence.
    pub fn rows(&self) -> impl Iterator<Item = (u32, &str, u8, u64, u32)> + '_ {
        self.entries.iter().map(|e| (e.dir, self.name(e), e.flags, e.size, e.mtime))
    }

    pub fn dirs(&self) -> &[String] {
        &self.dirs
    }

    /// Rebuild from persisted parts. Rows must reference valid dir ids.
    pub fn from_parts(dirs: Vec<String>, rows: impl Iterator<Item = (u32, String, u8, u64, u32)>) -> Self {
        let mut ix = Index { dirs, ..Index::default() };
        for (dir, name, flags, size, mtime) in rows {
            let off = ix.names.len() as u32;
            ix.names.push_str(&name);
            ix.entries.push(Entry { off, dir, len: name.len() as u16, flags, size, mtime });
        }
        ix.names.shrink_to_fit();
        ix.entries.shrink_to_fit();
        ix
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn build(roots: &[String]) -> Self {
        let out = Mutex::new(Vec::new());
        roots.par_iter().for_each(|r| scan(Path::new(r), 0, &out));
        let chunks = out.into_inner().unwrap();
        // exact capacities: no slack memory
        let mut ix = Index {
            dirs: Vec::with_capacity(chunks.len()),
            names: String::with_capacity(chunks.iter().map(|c| c.names.len()).sum()),
            entries: Vec::with_capacity(chunks.iter().map(|c| c.meta.len()).sum()),
        };
        for c in chunks {
            let dir = ix.dirs.len() as u32;
            let mut off = ix.names.len() as u32;
            ix.names.push_str(&c.names);
            for (len, flags, size, mtime) in c.meta {
                ix.entries.push(Entry { off, dir, len, flags, size, mtime });
                off += len as u32;
            }
            ix.dirs.push(c.dir);
        }
        ix
    }

    fn name(&self, e: &Entry) -> &str {
        &self.names[e.off as usize..e.off as usize + e.len as usize]
    }

    pub fn search(&self, q: &str, o: &Opts) -> Found {
        self.search_with(q, o, &|| false).unwrap_or_default()
    }

    /// Like `search`, but returns None as soon as `cancel()` turns true.
    pub fn search_with(&self, q: &str, o: &Opts, cancel: &(dyn Fn() -> bool + Sync)) -> Option<Found> {
        let q = Query::parse(q, o.case);
        if q.inc.is_empty() && q.ext.is_empty() {
            return Some(Found::default());
        }
        let mut v = self.run(&q, o, false, cancel);
        if v.is_empty() && q.inc.len() == 1 && q.inc[0].len() >= 3 && !cancel() {
            v = self.run(&q, o, true, cancel);
        }
        if cancel() {
            return None;
        }
        let total = v.len();
        if total > LIMIT {
            v.select_nth_unstable(LIMIT);
            v.truncate(LIMIT);
        }
        v.sort_unstable();
        let hits = v
            .iter()
            .map(|&(_, i)| {
                let e = &self.entries[i as usize];
                Hit { name: self.name(e).into(), dir: self.dirs[e.dir as usize].clone(), is_dir: e.flags & DIR != 0, size: e.size, mtime: e.mtime }
            })
            .collect();
        Some(Found { hits, total })
    }

    /// Returns (rank key, entry index). Rank: exact name < prefix < word start < inside a word < path-only match,
    /// then shorter names first.
    fn run(&self, q: &Query, o: &Opts, fuzzy: bool, cancel: &(dyn Fn() -> bool + Sync)) -> Vec<(u32, u32)> {
        let m = |h: &[u8], n: &[u8]| if fuzzy { subseq(h, n, o.case).then_some(1) } else { find(h, n, o.case) };
        self.entries
            .par_iter()
            .enumerate()
            .filter_map(|(i, e)| {
                if cancel() {
                    return None;
                }
                if (e.flags & HIDDEN != 0 && !o.hidden) || (e.flags & SYSTEM != 0 && !o.system) {
                    return None;
                }
                let name = self.name(e).as_bytes();
                if !q.ext.is_empty() && (e.flags & DIR != 0 || !q.ext.iter().any(|x| ext_of(name).eq_ignore_ascii_case(x))) {
                    return None;
                }
                if q.exc.iter().any(|x| find(name, x, o.case).is_some()) {
                    return None;
                }
                let dir = self.dirs[e.dir as usize].as_bytes();
                for t in &q.inc {
                    if m(name, t).is_none() && !(o.path && m(dir, t).is_some()) {
                        return None;
                    }
                }
                let score: u32 = match q.inc.first() {
                    None => 0,
                    Some(t) => match m(name, t) {
                        None => 4,
                        Some(0) if t.len() == name.len() => 0,
                        Some(0) => 1,
                        Some(p) if name[p - 1].is_ascii_alphanumeric() => 3,
                        Some(_) => 2,
                    },
                };
                Some((score << 16 | name.len().min(0xFFFF) as u32, i as u32))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_filters_and_ranking() {
        let root = std::env::temp_dir().join(format!("jetsearch-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("Reports/.hid")).unwrap();
        for f in ["Reports/budget report.txt", "Reports/report.pdf", "Reports/.hid/a report.txt", "notes.txt"] {
            fs::write(root.join(f), "").unwrap();
        }
        let ix = Index::build(&[root.to_string_lossy().into_owned()]);
        let o = Opts { case: false, path: false, hidden: true, system: true };
        let names = |q: &str, o: &Opts| ix.search(q, o).hits.into_iter().map(|h| h.name).collect::<Vec<_>>();

        assert_eq!(names("REPORT", &o), ["Reports", "report.pdf", "a report.txt", "budget report.txt"]);
        assert_eq!(names("report ext:txt -budget", &o), ["a report.txt"]);
        assert_eq!(names("reports pdf", &Opts { path: true, ..o }), ["report.pdf"]);
        assert_eq!(names("brpt", &o), ["budget report.txt"]); // fuzzy fallback
        #[cfg(unix)] // on Windows hidden means the file attribute, which a test can't set cheaply
        assert!(names("a report", &Opts { hidden: false, ..o }).is_empty());

        fs::remove_dir_all(&root).unwrap();
    }
}