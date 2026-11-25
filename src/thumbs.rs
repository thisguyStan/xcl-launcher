//! Loads game logos on background threads, scaled down to tile size, so the window never waits
//! for a PNG decoder.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};

/// Logos are decoded to at most this many pixels on a side (tiles are at most 320 points;
/// the GPU scales the rest).
const THUMB_PX: u32 = 384;

pub struct Thumbnails {
    textures: HashMap<String, TextureHandle>,
    /// Requested but not loaded yet (or failed): not requested again.
    requested: std::collections::HashSet<String>,
    queue: Arc<Mutex<Vec<(String, PathBuf)>>>,
    tx: Sender<(String, Option<ColorImage>)>,
    rx: Receiver<(String, Option<ColorImage>)>,
    workers: usize,
    busy: Arc<std::sync::atomic::AtomicUsize>,
}

impl Thumbnails {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        let workers = std::thread::available_parallelism().map_or(2, |n| n.get().clamp(1, 4));
        Thumbnails {
            textures: HashMap::new(),
            requested: Default::default(),
            queue: Arc::default(),
            tx,
            rx,
            workers,
            busy: Arc::default(),
        }
    }

    /// The logo of game `id`, or `None` while it is loading (or when it has none).
    pub fn get(
        &mut self,
        ctx: &egui::Context,
        id: &str,
        logo: Option<&PathBuf>,
    ) -> Option<&TextureHandle> {
        if !self.textures.contains_key(id) {
            if let Some(path) = logo
                && self.requested.insert(id.to_owned())
            {
                self.queue
                    .lock()
                    .expect("thumbnail queue")
                    .push((id.to_owned(), path.clone()));
                self.spawn_worker(ctx);
            }
            return None;
        }
        self.textures.get(id)
    }

    /// Takes in the logos decoded since the last frame.
    pub fn receive(&mut self, ctx: &egui::Context) {
        while let Ok((id, image)) = self.rx.try_recv() {
            if let Some(image) = image {
                let tex = ctx.load_texture(format!("logo:{id}"), image, TextureOptions::LINEAR);
                self.textures.insert(id, tex);
            }
        }
    }

    /// Forgets everything (after a rescan the logos may have changed).
    pub fn clear(&mut self) {
        self.textures.clear();
        self.requested.clear();
        self.queue.lock().expect("thumbnail queue").clear();
    }

    fn spawn_worker(&self, ctx: &egui::Context) {
        use std::sync::atomic::Ordering;
        if self.busy.load(Ordering::Acquire) >= self.workers {
            return;
        }
        self.busy.fetch_add(1, Ordering::AcqRel);
        let (queue, tx, busy, ctx) = (
            self.queue.clone(),
            self.tx.clone(),
            self.busy.clone(),
            ctx.clone(),
        );
        let workers = self.workers;
        std::thread::spawn(move || {
            loop {
                loop {
                    let job = queue.lock().expect("thumbnail queue").pop();
                    let Some((id, path)) = job else { break };
                    let image = decode(&path);
                    if tx.send((id, image)).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
                busy.fetch_sub(1, Ordering::AcqRel);
                // a request that came in while this worker was leaving found no free slot
                if queue.lock().expect("thumbnail queue").is_empty() {
                    break;
                }
                if busy.fetch_add(1, Ordering::AcqRel) >= workers {
                    busy.fetch_sub(1, Ordering::AcqRel);
                    break;
                }
            }
        });
    }
}

fn decode(path: &PathBuf) -> Option<ColorImage> {
    let img = image::open(path).ok()?;
    let img = if img.width() > THUMB_PX || img.height() > THUMB_PX {
        img.thumbnail(THUMB_PX, THUMB_PX)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    Some(ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        rgba.as_raw(),
    ))
}
