//! SQLite persistence for the filename index. Every write is one transaction,
//! so the previous valid state survives crashes and interruptions.
//! The index is a rebuildable cache: schema upgrades below v3 drop it and rescan.
use crate::index::{Diff, Index};
use rusqlite::{params, Connection};
use std::{collections::HashMap, path::Path};

const SCHEMA_VERSION: i64 = 3;

pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let c = Connection::open(path)?;
    c.pragma_update(None, "journal_mode", "WAL")?;
    c.pragma_update(None, "synchronous", "NORMAL")?;
    migrate(&c)?;
    Ok(c)
}

fn migrate(c: &Connection) -> rusqlite::Result<()> {
    let v: i64 = c.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if v > SCHEMA_VERSION {
        return Err(rusqlite::Error::InvalidParameterName(format!("db schema {v} newer than app {SCHEMA_VERSION}")));
    }
    if v < 3 {
        c.execute_batch(&format!(
            "BEGIN;
             DROP TABLE IF EXISTS entries; DROP TABLE IF EXISTS dirs; DROP TABLE IF EXISTS meta;
             CREATE TABLE dirs(id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE);
             CREATE TABLE entries(dir INTEGER NOT NULL, name TEXT NOT NULL, flags INTEGER NOT NULL,
                size INTEGER NOT NULL, mtime INTEGER NOT NULL, PRIMARY KEY(dir, name)) WITHOUT ROWID;
             CREATE TABLE meta(k TEXT PRIMARY KEY, v TEXT NOT NULL);
             PRAGMA user_version={SCHEMA_VERSION};
             COMMIT;"
        ))?;
    }
    Ok(())
}

fn stamp(tx: &rusqlite::Transaction, t: i64) -> rusqlite::Result<()> {
    tx.execute("INSERT OR REPLACE INTO meta(k,v) VALUES('last_scan',?)", params![t.to_string()]).map(|_| ())
}

/// Replace everything (first scan, or recovery after a failed write).
pub fn save(c: &mut Connection, ix: &Index, unix_time: i64) -> rusqlite::Result<()> {
    let tx = c.transaction()?;
    tx.execute_batch("DELETE FROM entries; DELETE FROM dirs;")?;
    {
        let mut d = tx.prepare("INSERT INTO dirs(id,path) VALUES(?,?)")?;
        for (i, p) in ix.dirs().iter().enumerate() {
            d.execute(params![i as i64 + 1, p])?;
        }
        let mut e = tx.prepare("INSERT INTO entries(dir,name,flags,size,mtime) VALUES(?,?,?,?,?)")?;
        for (dir, name, flags, size, mtime) in ix.rows() {
            e.execute(params![dir as i64 + 1, name, flags as i64, size as i64, mtime as i64])?;
        }
    }
    stamp(&tx, unix_time)?;
    tx.commit()
}

/// Apply only the changes between two scans. Caller guarantees the DB matches the `old` snapshot.
pub fn apply(c: &mut Connection, d: &Diff, unix_time: i64) -> rusqlite::Result<()> {
    let tx = c.transaction()?;
    {
        let mut del_dir_entries = tx.prepare("DELETE FROM entries WHERE dir=(SELECT id FROM dirs WHERE path=?)")?;
        let mut del_dir = tx.prepare("DELETE FROM dirs WHERE path=?")?;
        for p in &d.gone_dirs {
            del_dir_entries.execute([p])?;
            del_dir.execute([p])?;
        }
        let mut del = tx.prepare("DELETE FROM entries WHERE dir=(SELECT id FROM dirs WHERE path=?1) AND name=?2")?;
        for (dir, name) in &d.removed {
            del.execute(params![dir, name])?;
        }
        let mut add_dir = tx.prepare("INSERT OR IGNORE INTO dirs(path) VALUES(?)")?;
        let mut up = tx.prepare(
            "INSERT OR REPLACE INTO entries(dir,name,flags,size,mtime)
             VALUES((SELECT id FROM dirs WHERE path=?1),?2,?3,?4,?5)",
        )?;
        for r in &d.upsert {
            add_dir.execute([&r.dir])?;
            up.execute(params![r.dir, r.name, r.flags as i64, r.size as i64, r.mtime as i64])?;
        }
    }
    stamp(&tx, unix_time)?;
    tx.commit()
}

/// Returns None if nothing usable is stored.
pub fn load(c: &Connection) -> rusqlite::Result<Option<Index>> {
    let mut d = c.prepare("SELECT id,path FROM dirs ORDER BY id")?;
    let mut dirs = Vec::new();
    let mut map: HashMap<i64, u32> = HashMap::new();
    for r in d.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
        let (id, p) = r?;
        map.insert(id, dirs.len() as u32);
        dirs.push(p);
    }
    if dirs.is_empty() {
        return Ok(None);
    }
    let mut e = c.prepare("SELECT dir,name,flags,size,mtime FROM entries")?;
    let mut rows = Vec::new();
    for r in e.query_map([], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?, r.get::<_, i64>(4)?))
    })? {
        let (dir, name, f, sz, mt) = r?;
        let Some(&di) = map.get(&dir) else { return Ok(None) }; // dangling dir id: force rescan
        rows.push((di, name, f as u8, sz as u64, mt as u32));
    }
    Ok(Some(Index::from_parts(dirs, rows.into_iter())))
}

pub fn last_scan(c: &Connection) -> Option<i64> {
    c.query_row("SELECT v FROM meta WHERE k='last_scan'", [], |r| r.get::<_, String>(0)).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{diff, Opts};
    use std::{collections::BTreeSet, fs, path::PathBuf};

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("jetsearch-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }
    fn build(root: &Path) -> Index {
        Index::build(&[root.to_string_lossy().into_owned()])
    }
    fn rowset(ix: &Index) -> BTreeSet<(String, String, u8, u64, u32)> {
        ix.rows().map(|(d, n, f, s, m)| (ix.dirs()[d as usize].clone(), n.to_string(), f, s, m)).collect()
    }
    const ALL: Opts = Opts { case: false, path: false, hidden: true, system: true };

    #[test]
    fn roundtrip_and_reopen() {
        let base = tmp("rt");
        let root = base.join("data");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub/alpha report.txt"), vec![0u8; 1234]).unwrap();
        fs::write(root.join("beta.pdf"), "").unwrap();
        let ix = build(&root);
        let dbp = base.join("db/jet.sqlite");
        let mut c = open(&dbp).unwrap();
        assert!(load(&c).unwrap().is_none());
        save(&mut c, &ix, 42).unwrap();
        drop(c);
        let c = open(&dbp).unwrap(); // simulated restart
        let back = load(&c).unwrap().expect("stored");
        assert_eq!(last_scan(&c), Some(42));
        assert_eq!(rowset(&back), rowset(&ix)); // includes size + mtime
        let h = back.search("alpha", &ALL).hits.remove(0);
        assert_eq!((h.name.as_str(), h.size), ("alpha report.txt", 1234));
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn old_schema_is_reset_not_corrupted() {
        let base = tmp("mg");
        let dbp = base.join("old.sqlite");
        {
            let c = Connection::open(&dbp).unwrap();
            c.execute_batch(
                "CREATE TABLE dirs(id INTEGER PRIMARY KEY, path TEXT NOT NULL);
                 CREATE TABLE entries(dir INTEGER NOT NULL, name TEXT NOT NULL, flags INTEGER NOT NULL);
                 INSERT INTO dirs VALUES(0,'/x'); INSERT INTO entries VALUES(0,'old.txt',0);
                 PRAGMA user_version=2;",
            )
            .unwrap();
        }
        let c = open(&dbp).unwrap();
        assert!(load(&c).unwrap().is_none()); // cache dropped, rescan rebuilds
        assert_eq!(c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), SCHEMA_VERSION);
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn incremental_apply_matches_fresh_scan() {
        let base = tmp("inc");
        let root = base.join("data");
        fs::create_dir_all(root.join("keep")).unwrap();
        fs::create_dir_all(root.join("gone/deep")).unwrap();
        fs::write(root.join("keep/a.txt"), "aaa").unwrap();
        fs::write(root.join("keep/del.txt"), "x").unwrap();
        fs::write(root.join("gone/deep/z.txt"), "z").unwrap();
        fs::write(root.join("top.txt"), "t").unwrap();
        let a = build(&root);
        let mut c = open(&base.join("i.sqlite")).unwrap();
        save(&mut c, &a, 1).unwrap();

        fs::remove_file(root.join("keep/del.txt")).unwrap(); // removed
        fs::write(root.join("keep/a.txt"), "much longer now").unwrap(); // changed (size)
        fs::create_dir_all(root.join("fresh")).unwrap(); // new dir
        fs::write(root.join("fresh/n.txt"), "n").unwrap(); // added
        fs::remove_dir_all(root.join("gone")).unwrap(); // whole dir tree removed
        let b = build(&root);

        let d = diff(&a, &b);
        // added: fresh (dir) + n.txt; changed: a.txt (+ keep/root dir mtimes may also change)
        assert!(d.added >= 2 && d.changed >= 1);
        // deleted: del.txt, gone (dir), deep (dir), z.txt
        assert_eq!(d.deleted, 4);
        assert_eq!(d.gone_dirs.len(), 2);
        apply(&mut c, &d, 2).unwrap();
        assert_eq!(last_scan(&c), Some(2));
        let back = load(&c).unwrap().unwrap();
        assert_eq!(rowset(&back), rowset(&b));
        assert!(diff(&b, &b).is_empty());
        // applying again from the stored state on an unchanged tree is a no-op
        assert!(diff(&back, &b).is_empty());
        fs::remove_dir_all(&base).unwrap();
    }
}