//! Background search worker. Keeps only the newest request, cancels the one in flight,
//! and never delivers a result for a superseded generation.
use crate::index::{Found, Index, Opts};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Condvar, Mutex, RwLock,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

struct Req {
    gen: u64,
    query: String,
    opts: Opts,
}

struct Inner {
    slot: Mutex<Option<Req>>,
    cv: Condvar,
    latest: Arc<AtomicU64>,
    stop: AtomicBool,
}

pub struct SearchWorker {
    inner: Arc<Inner>,
    handle: Option<JoinHandle<()>>,
}

impl SearchWorker {
    /// `latest` is the shared newest-generation counter (the consumer can read it to drop stale results).
    /// `deliver(gen, found, elapsed)` runs on the worker thread, only for the newest generation.
    pub fn new<F>(index: Arc<RwLock<Arc<Index>>>, latest: Arc<AtomicU64>, deliver: F) -> Self
    where
        F: Fn(u64, Found, Duration) + Send + 'static,
    {
        let inner = Arc::new(Inner {
            slot: Mutex::new(None),
            cv: Condvar::new(),
            latest,
            stop: AtomicBool::new(false),
        });
        let w = inner.clone();
        let handle = std::thread::Builder::new()
            .name("jetsearch-search".into())
            .spawn(move || loop {
                let req = {
                    let mut g = w.slot.lock().unwrap();
                    loop {
                        if w.stop.load(Ordering::SeqCst) {
                            return;
                        }
                        if let Some(r) = g.take() {
                            break r;
                        }
                        g = w.cv.wait(g).unwrap();
                    }
                };
                let ix = index.read().unwrap().clone(); // short lock: Arc clone only
                let t = Instant::now();
                let cancel = || w.latest.load(Ordering::Relaxed) != req.gen || w.stop.load(Ordering::Relaxed);
                if let Some(found) = ix.search_with(&req.query, &req.opts, &cancel) {
                    if w.latest.load(Ordering::SeqCst) == req.gen {
                        deliver(req.gen, found, t.elapsed());
                    }
                }
            })
            .expect("spawn search worker");
        SearchWorker { inner, handle: Some(handle) }
    }

    /// Queue a query; supersedes and cancels any earlier one. Returns its generation.
    pub fn submit(&self, query: String, opts: Opts) -> u64 {
        let mut g = self.inner.slot.lock().unwrap();
        let gen = self.inner.latest.fetch_add(1, Ordering::SeqCst) + 1;
        *g = Some(Req { gen, query, opts });
        self.inner.cv.notify_one();
        gen
    }

    /// Shared handle to the newest generation, for dropping stale results on the consumer side too.
    pub fn latest(&self) -> Arc<AtomicU64> {
        self.inner.latest.clone()
    }
}

impl Drop for SearchWorker {
    fn drop(&mut self) {
        self.inner.stop.store(true, Ordering::SeqCst);
        self.inner.cv.notify_all();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    const O: Opts = Opts { case: false, path: false, hidden: true, system: true };

    fn big_index(n: usize) -> Index {
        let rows = (0..n).map(|i| (0u32, format!("file{i}-report.txt"), 0u8, 0u64, 0u32));
        Index::from_parts(vec!["/d".into()], rows)
    }

    #[test]
    fn cancelled_search_returns_none() {
        let ix = big_index(50_000);
        assert!(ix.search_with("report", &O, &|| true).is_none());
        assert!(ix.search_with("report", &O, &|| false).is_some());
    }

    #[test]
    fn only_newest_generation_wins_and_order_is_monotonic() {
        let shared: Arc<RwLock<Arc<Index>>> = Arc::new(RwLock::new(Arc::new(big_index(200_000))));
        let (tx, rx) = mpsc::channel();
        let w = SearchWorker::new(shared, Arc::new(AtomicU64::new(0)), move |g, f, _| {
            tx.send((g, f.total)).unwrap();
        });
        let mut last = 0;
        for i in 0..60 {
            // later queries match fewer files, so totals identify which query produced a result
            last = w.submit(if i == 59 { "file1999".into() } else { format!("file{i}") }, O);
        }
        let mut got = vec![];
        while let Ok((g, t)) = rx.recv_timeout(Duration::from_secs(10)) {
            got.push((g, t));
            if g == last {
                break;
            }
        }
        assert_eq!(got.last().map(|x| x.0), Some(last), "newest generation must be delivered");
        assert!(got.windows(2).all(|w| w[0].0 < w[1].0), "no stale or reordered delivery");
        // "file1999" matches file1999, file19990..19999, file199900..199999 = 1 + 10 + 100
        assert_eq!(got.last().unwrap().1, 111);
        drop(w); // joins cleanly
        assert!(rx.try_recv().is_err());
    }
}