//! Compact in-memory file index: one string arena for all names, 12-byte entries,
//! one path string per directory. Searching is a parallel scan over the entries.
use rayon::prelude::*;
use std::{fs, path::Path, sync::Mutex};

const DIR: u8 = 1;
const HIDDEN: u8 = 2;
const SYSTEM: u8 = 4;
const LIMIT: usize = 200;

struct Entry {
    off: u32,
    dir: u32,
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
    meta: Vec<(u16, u8)>,
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
}

impl Hit {
    pub fn full(&self) -> String {
        Path::new(&self.dir).join(&self.name).to_string_lossy().into_owned()
    }
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
fn attrs(e: &fs::DirEntry, _: &str) -> u8 {
    use std::os::windows::fs::MetadataExt;
    let a = e.metadata().map_or(0, |m| m.file_attributes());
    (if a & 0x2 != 0 { HIDDEN } else { 0 }) | (if a & 0x4 != 0 { SYSTEM } else { 0 })
}

#[cfg(not(windows))]
fn attrs(_: &fs::DirEntry, name: &str) -> u8 {
    if name.starts_with('.') { HIDDEN } else { 0 }
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
        let mut flags = attrs(&e, &name) | inherit;
        // file_type() does not follow symlinks/junctions, so there are no directory loops
        if ft.is_dir() {
            subs.push((e.path(), flags & (HIDDEN | SYSTEM)));
            flags |= DIR;
        }
        c.meta.push((name.len() as u16, flags));
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
            for (len, flags) in c.meta {
                ix.entries.push(Entry { off, dir, len, flags });
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
        let q = Query::parse(q, o.case);
        if q.inc.is_empty() && q.ext.is_empty() {
            return Found::default();
        }
        let mut v = self.run(&q, o, false);
        if v.is_empty() && q.inc.len() == 1 && q.inc[0].len() >= 3 {
            v = self.run(&q, o, true);
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
                Hit { name: self.name(e).into(), dir: self.dirs[e.dir as usize].clone(), is_dir: e.flags & DIR != 0 }
            })
            .collect();
        Found { hits, total }
    }

    /// Returns (rank key, entry index). Rank: exact name < prefix < word start < inside a word < path-only match,
    /// then shorter names first.
    fn run(&self, q: &Query, o: &Opts, fuzzy: bool) -> Vec<(u32, u32)> {
        let m = |h: &[u8], n: &[u8]| if fuzzy { subseq(h, n, o.case).then_some(1) } else { find(h, n, o.case) };
        self.entries
            .par_iter()
            .enumerate()
            .filter_map(|(i, e)| {
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
