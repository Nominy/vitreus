//! Background decoding with a priority queue the UI rewrites on every navigation.
//!
//! The UI owns *what* is wanted; the pool only guarantees that the most
//! important pending request is the next one picked up. Requests already being
//! decoded are never queued twice; stale ones simply drop out of the queue.

use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{Arc, Condvar, Mutex},
    thread,
};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};

use crate::decode::{self, Decoded};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Tiny image for the film strip and as an instant stand-in while flying through.
    Thumb,
    /// Screen-sized image; what is normally on screen.
    Preview,
    /// Native resolution, only requested once zoomed past the preview.
    Full,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Request {
    pub path: PathBuf,
    pub kind: Kind,
}

pub struct Loaded {
    pub path: PathBuf,
    pub kind: Kind,
    pub result: Result<Decoded, String>,
}

#[derive(Default)]
struct Queue {
    /// Highest priority last, so workers can `pop`.
    pending: Vec<Request>,
    in_flight: HashSet<Request>,
    shutdown: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
}

pub struct Loader {
    shared: Arc<Shared>,
}

impl Loader {
    /// `preview_size` is the device-pixel box previews are downscaled into.
    pub fn new(preview_size: (u32, u32)) -> (Self, UnboundedReceiver<Loaded>) {
        let shared = Arc::new(Shared {
            queue: Mutex::default(),
            wake: Condvar::new(),
        });
        let (tx, rx) = unbounded();
        let workers = thread::available_parallelism()
            .map_or(4, |n| n.get())
            .saturating_sub(1)
            .clamp(2, 6);
        for i in 0..workers {
            let shared = shared.clone();
            let tx = tx.clone();
            thread::Builder::new()
                .name(format!("decode-{i}"))
                .spawn(move || worker(&shared, &tx, preview_size))
                .expect("spawn decoder thread");
        }
        (Self { shared }, rx)
    }

    /// Replaces everything not yet started. `requests` is in priority order,
    /// most important first.
    pub fn set_queue(&self, requests: Vec<Request>) {
        let mut queue = self.shared.queue.lock().unwrap();
        let in_flight = &queue.in_flight;
        let mut pending: Vec<Request> = requests
            .into_iter()
            .filter(|r| !in_flight.contains(r))
            .collect();
        pending.reverse();
        queue.pending = pending;
        drop(queue);
        self.shared.wake.notify_all();
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        let mut queue = self.shared.queue.lock().unwrap();
        queue.shutdown = true;
        queue.pending.clear();
        drop(queue);
        self.shared.wake.notify_all();
    }
}

fn worker(shared: &Shared, tx: &UnboundedSender<Loaded>, preview_size: (u32, u32)) {
    loop {
        let request = {
            let mut queue = shared.queue.lock().unwrap();
            loop {
                if queue.shutdown {
                    return;
                }
                if let Some(request) = queue.pending.pop() {
                    queue.in_flight.insert(request.clone());
                    break request;
                }
                queue = shared.wake.wait(queue).unwrap();
            }
        };

        // A corrupt file must not take a worker (and its in-flight slot) down with it.
        let result = std::panic::catch_unwind(|| match request.kind {
            Kind::Thumb => decode::thumbnail(&request.path),
            Kind::Preview => decode::preview(&request.path, preview_size),
            Kind::Full => decode::full(&request.path),
        })
        .unwrap_or_else(|_| Err(anyhow::anyhow!("decoder crashed on this file")))
        .map_err(|e| e.to_string());

        shared.queue.lock().unwrap().in_flight.remove(&request);
        let loaded = Loaded {
            path: request.path,
            kind: request.kind,
            result,
        };
        if tx.unbounded_send(loaded).is_err() {
            return;
        }
    }
}
