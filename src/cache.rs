//! Preview cache, priority queue and render workers (DESIGN.md §6.3).
//!
//! * The cache key is `(path, mtime, len, target_px)`. Entries are stored under
//!   `(path, target_px)` together with the file's mtime and length when it was rendered,
//!   and an entry only counts as a hit if they still match.
//! * A byte-bounded LRU holds protocol-ready previews: kitty transmit strings or encoded
//!   ratatui-image protocols.
//! * N workers (cores − 1) take jobs from a priority queue. The UI replaces the queue on
//!   every selection change, and jobs already running finish and land in the cache.
//! * Results come back to the UI over a crossbeam channel, so the UI thread never waits on
//!   a render.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::SystemTime;

use crossbeam_channel::{Receiver, Sender};
use image::DynamicImage;
use lru::LruCache;
use ratatui::layout::Size;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::Protocol;
use ratatui_image::Resize;

use crate::render::kitty::KittyImage;
use crate::render::{self, Kind, Unavailable};

/// What the terminal will display for a file.
#[derive(Clone)]
pub enum Preview {
    /// Our own kitty encoding (see `render::kitty`).
    Kitty(Arc<KittyImage>),
    /// Sixel / iTerm2 / halfblocks (or kitty inside tmux) through ratatui-image.
    Proto(Arc<Protocol>),
    /// Text card: path, size, mtime and an optional note.
    Card(Arc<Vec<String>>),
}

impl Preview {
    fn byte_size(&self) -> usize {
        match self {
            Preview::Kitty(k) => k.byte_size(),
            // The encoded data is private to ratatui-image; estimate from the cell size.
            Preview::Proto(p) => {
                let s = p.size();
                (s.width as usize * s.height as usize).max(1) * 2048
            }
            Preview::Card(c) => c.iter().map(|l| l.len() + 24).sum::<usize>() + 64,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JobKey {
    pub path: Arc<Path>,
    pub target_px: (u32, u32),
}

/// File identity used to tell whether a cached render is stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub mtime: Option<SystemTime>,
    pub len: u64,
}

impl Stamp {
    pub fn of(path: &Path) -> Option<Stamp> {
        std::fs::metadata(path).ok().map(|m| Stamp {
            mtime: m.modified().ok(),
            len: m.len(),
        })
    }
}

#[derive(Clone)]
struct Entry {
    stamp: Option<Stamp>,
    preview: Preview,
    bytes: usize,
}

/// Everything a worker needs to turn pixels into a protocol-ready preview.
#[derive(Clone)]
pub struct Encoder {
    pub picker: Picker,
    /// Use our own kitty encoder (kitty protocol, not inside tmux).
    pub own_kitty: bool,
    pub kitty_compress: bool,
    pub cell_px: (u16, u16),
    pub area_cells: (u16, u16),
}

impl Encoder {
    pub fn new(picker: Picker) -> Encoder {
        let own_kitty = picker.protocol_type() == ProtocolType::Kitty && !picker.tmux_detected();
        let kitty_compress = picker
            .capabilities()
            .contains(&ratatui_image::picker::Capability::KittyCompression);
        let fs = picker.font_size();
        Encoder {
            picker,
            own_kitty,
            kitty_compress,
            cell_px: (fs.width.max(1), fs.height.max(1)),
            area_cells: (0, 0),
        }
    }

    fn encode(&self, img: image::RgbaImage) -> anyhow::Result<Preview> {
        if self.own_kitty {
            Ok(Preview::Kitty(Arc::new(KittyImage::encode(
                &img,
                self.cell_px,
                self.kitty_compress,
            ))))
        } else {
            let size = Size::new(self.area_cells.0.max(1), self.area_cells.1.max(1));
            let p = self
                .picker
                .new_protocol(DynamicImage::ImageRgba8(img), size, Resize::Fit(None))
                .map_err(|e| anyhow::anyhow!("encoding preview: {e}"))?;
            Ok(Preview::Proto(Arc::new(p)))
        }
    }
}

#[derive(Clone)]
struct Job {
    key: JobKey,
    kind: Kind,
    gen: u64,
    enc: Arc<Encoder>,
}

pub struct JobResult {
    key: JobKey,
    gen: u64,
    stamp: Option<Stamp>,
    preview: Preview,
    /// Set when a backend is missing (PDFium), so the UI can say so once.
    pub unavailable: Option<String>,
}

#[derive(Default)]
struct QueueState {
    /// Pending jobs, highest priority first.
    pending: Vec<Job>,
    running: HashSet<JobKey>,
    shutdown: bool,
}

struct Shared {
    q: Mutex<QueueState>,
    cv: Condvar,
}

pub struct Cache {
    lru: LruCache<JobKey, Entry>,
    bytes: usize,
    budget: usize,
    shared: Arc<Shared>,
    results: Receiver<JobResult>,
    workers: Vec<std::thread::JoinHandle<()>>,
    /// Bumped whenever the target size changes; results from older generations are dropped.
    gen: u64,
    pub enc: Arc<Encoder>,
}

/// What the UI should show for a request.
pub enum Lookup {
    Ready(Preview),
    Pending,
}

impl Cache {
    pub fn new(budget_bytes: usize, enc: Encoder, workers: usize) -> Cache {
        let shared = Arc::new(Shared {
            q: Mutex::new(QueueState::default()),
            cv: Condvar::new(),
        });
        let (tx, rx) = crossbeam_channel::unbounded();
        let workers = (0..workers.max(1))
            .map(|i| {
                let shared = shared.clone();
                let tx = tx.clone();
                std::thread::Builder::new()
                    .name(format!("render-{i}"))
                    .spawn(move || worker(shared, tx))
                    .expect("spawning render worker")
            })
            .collect();
        Cache {
            lru: LruCache::unbounded(),
            bytes: 0,
            budget: budget_bytes.max(1),
            shared,
            results: rx,
            workers,
            gen: 0,
            enc: Arc::new(enc),
        }
    }

    pub fn default_workers() -> usize {
        std::thread::available_parallelism()
            .map(|n| n.get().saturating_sub(1))
            .unwrap_or(1)
            .max(1)
    }

    pub fn results(&self) -> &Receiver<JobResult> {
        &self.results
    }

    #[cfg(test)]
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.lru.len()
    }

    /// Change the preview geometry. Renders made for the old size are thrown away.
    /// Set the preview geometry. Returns `true` if it changed, which discards all renders
    /// and queued jobs; the caller must request again.
    pub fn set_geometry(&mut self, area_cells: (u16, u16), cell_px: (u16, u16)) -> bool {
        if self.enc.area_cells == area_cells && self.enc.cell_px == cell_px {
            return false;
        }
        let mut enc = (*self.enc).clone();
        enc.area_cells = area_cells;
        enc.cell_px = cell_px;
        self.enc = Arc::new(enc);
        self.gen += 1;
        self.lru.clear();
        self.bytes = 0;
        self.shared.q.lock().unwrap().pending.clear();
        true
    }

    pub fn target_px(&self) -> (u32, u32) {
        let e = &self.enc;
        (
            e.area_cells.0 as u32 * e.cell_px.0 as u32,
            e.area_cells.1 as u32 * e.cell_px.1 as u32,
        )
    }

    fn key(&self, path: &Path) -> JobKey {
        JobKey {
            path: Arc::from(path),
            target_px: self.target_px(),
        }
    }

    /// Look up the preview for the selected file. `verify` re-stats the file, so a file
    /// rewritten since its render is not served stale.
    pub fn get(&mut self, path: &Path, verify: bool) -> Lookup {
        let key = self.key(path);
        let fresh = match self.lru.peek(&key) {
            None => return Lookup::Pending,
            Some(e) => !verify || e.stamp == Stamp::of(path),
        };
        if !fresh {
            if let Some(e) = self.lru.pop(&key) {
                self.bytes -= e.bytes;
            }
            return Lookup::Pending;
        }
        Lookup::Ready(self.lru.get(&key).expect("checked above").preview.clone())
    }

    fn contains(&self, key: &JobKey) -> bool {
        self.lru.contains(key)
    }

    fn insert(&mut self, key: JobKey, stamp: Option<Stamp>, preview: Preview) {
        let bytes = preview.byte_size();
        if let Some(old) = self.lru.put(
            key,
            Entry {
                stamp,
                preview,
                bytes,
            },
        ) {
            self.bytes -= old.bytes;
        }
        self.bytes += bytes;
        // Keep at least the newest entry, even if it alone exceeds the budget.
        while self.bytes > self.budget && self.lru.len() > 1 {
            match self.lru.pop_lru() {
                Some((_, e)) => self.bytes -= e.bytes,
                None => break,
            }
        }
    }

    /// Replace the queue with `paths`, highest priority first. Cached and running files are
    /// skipped. Files no renderer can handle get their text card right away.
    pub fn request(&mut self, paths: &[PathBuf]) {
        if self.enc.area_cells.0 == 0 || self.enc.area_cells.1 == 0 {
            return;
        }
        let mut jobs = Vec::with_capacity(paths.len());
        let mut seen = HashSet::with_capacity(paths.len());
        for p in paths {
            let key = self.key(p);
            if self.contains(&key) || !seen.insert(key.clone()) {
                continue;
            }
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            match render::kind_for_ext(ext) {
                None => {
                    let card = Preview::Card(Arc::new(render::text_card(p, None)));
                    self.insert(key, Stamp::of(p), card);
                }
                Some(kind) => jobs.push(Job {
                    key,
                    kind,
                    gen: self.gen,
                    enc: self.enc.clone(),
                }),
            }
        }
        let mut q = self.shared.q.lock().unwrap();
        q.pending = jobs
            .into_iter()
            .filter(|j| !q.running.contains(&j.key))
            .collect();
        if !q.pending.is_empty() {
            self.shared.cv.notify_all();
        }
    }

    /// Store a finished render. Returns `true` if it belongs to the current geometry.
    pub fn accept(&mut self, r: JobResult) -> bool {
        if r.gen != self.gen {
            return false;
        }
        self.insert(r.key, r.stamp, r.preview);
        true
    }

    #[cfg(test)]
    /// Number of jobs waiting plus running (for tests and benchmarks).
    pub fn outstanding(&self) -> usize {
        let q = self.shared.q.lock().unwrap();
        q.pending.len() + q.running.len()
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        {
            let mut q = self.shared.q.lock().unwrap();
            q.shutdown = true;
            q.pending.clear();
        }
        self.shared.cv.notify_all();
        // Workers finish their current render and exit. Don't wait for slow renders.
        for w in self.workers.drain(..) {
            if w.is_finished() {
                let _ = w.join();
            }
        }
    }
}

fn worker(shared: Arc<Shared>, tx: Sender<JobResult>) {
    loop {
        let job = {
            let mut q = shared.q.lock().unwrap();
            loop {
                if q.shutdown {
                    return;
                }
                if !q.pending.is_empty() {
                    let job = q.pending.remove(0);
                    q.running.insert(job.key.clone());
                    break job;
                }
                q = shared.cv.wait(q).unwrap();
            }
        };
        let result = run_job(&job);
        // Leave `running` before the result is visible: otherwise a `request` in between
        // would skip the file as running, and nothing would ever queue it again.
        shared.q.lock().unwrap().running.remove(&job.key);
        let _ = tx.send(result);
    }
}

fn run_job(job: &Job) -> JobResult {
    let path = &job.key.path;
    let stamp = Stamp::of(path);
    let rendered = render::renderer_for(job.kind)
        .render(path, job.key.target_px)
        .and_then(|img| job.enc.encode(img));
    let (preview, unavailable) = match rendered {
        Ok(p) => (p, None),
        Err(e) => {
            if let Some(u) = e.downcast_ref::<Unavailable>() {
                let note = format!("PDF previews unavailable: {u}");
                (
                    Preview::Card(Arc::new(render::text_card(path, Some(&note)))),
                    Some(u.0.clone()),
                )
            } else {
                let note = format!("preview failed: {e:#}");
                (
                    Preview::Card(Arc::new(render::text_card(path, Some(&note)))),
                    None,
                )
            }
        }
    };
    JobResult {
        key: job.key.clone(),
        gen: job.gen,
        stamp,
        preview,
        unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn cache(budget: usize) -> Cache {
        let mut c = Cache::new(budget, Encoder::new(Picker::halfblocks()), 2);
        c.set_geometry((20, 10), (10, 20));
        c
    }

    fn wait_all(c: &mut Cache, n: usize) -> usize {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut got = 0;
        while got < n && Instant::now() < deadline {
            if let Ok(r) = c.results().recv_timeout(Duration::from_millis(100)) {
                if c.accept(r) {
                    got += 1;
                }
            }
        }
        got
    }

    fn png(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        image::RgbaImage::from_pixel(64, 32, image::Rgba([200, 10, 10, 255]))
            .save(&p)
            .unwrap();
        p
    }

    #[test]
    fn renders_caches_and_detects_staleness() {
        let tmp = tempfile::tempdir().unwrap();
        let a = png(tmp.path(), "a.png");
        let txt = tmp.path().join("notes.txt");
        std::fs::write(&txt, "hi").unwrap();
        let mut c = cache(64 << 20);
        assert!(matches!(c.get(&a, true), Lookup::Pending));
        c.request(&[a.clone(), txt.clone()]);
        // Text cards are immediate.
        assert!(matches!(c.get(&txt, true), Lookup::Ready(Preview::Card(_))));
        assert_eq!(wait_all(&mut c, 1), 1);
        assert!(matches!(c.get(&a, true), Lookup::Ready(Preview::Proto(_))));
        // Re-requesting a cached file queues nothing.
        c.request(std::slice::from_ref(&a));
        assert_eq!(c.outstanding(), 0);
        // Rewriting the file (different length) invalidates it.
        image::RgbaImage::from_pixel(65, 32, image::Rgba([0, 0, 0, 255]))
            .save(&a)
            .unwrap();
        assert!(matches!(c.get(&a, true), Lookup::Pending));
    }

    #[test]
    fn geometry_change_discards_old_renders() {
        let tmp = tempfile::tempdir().unwrap();
        let a = png(tmp.path(), "a.png");
        let mut c = cache(64 << 20);
        c.request(std::slice::from_ref(&a));
        assert_eq!(wait_all(&mut c, 1), 1);
        c.set_geometry((30, 10), (10, 20));
        assert_eq!(c.len(), 0);
        assert!(matches!(c.get(&a, false), Lookup::Pending));
    }

    #[test]
    fn byte_budget_evicts_lru() {
        let tmp = tempfile::tempdir().unwrap();
        let paths: Vec<PathBuf> = (0..6)
            .map(|i| png(tmp.path(), &format!("{i}.png")))
            .collect();
        // Each halfblock preview is estimated at 20*10*2048 bytes (~400 KB).
        let mut c = cache(1 << 20);
        c.request(&paths);
        assert_eq!(wait_all(&mut c, 6), 6);
        assert!(c.bytes() <= 1 << 20);
        assert!(c.len() >= 1 && c.len() < 6);
    }

    #[test]
    fn broken_files_get_cards() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("bad.png");
        std::fs::write(&p, b"nope").unwrap();
        let mut c = cache(64 << 20);
        c.request(std::slice::from_ref(&p));
        assert_eq!(wait_all(&mut c, 1), 1);
        match c.get(&p, false) {
            Lookup::Ready(Preview::Card(lines)) => {
                assert!(lines.iter().any(|l| l.starts_with("preview failed")))
            }
            _ => panic!("expected a card"),
        }
    }
}
