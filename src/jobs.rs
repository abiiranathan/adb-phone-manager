//! Background job queue for Extras features.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;

use eframe::egui;

use crate::adb::AdbError;

pub type BoxedMerge<T> = Box<dyn FnOnce(&mut T) + Send + 'static>;
pub type JobResult<T> = Result<BoxedMerge<T>, AdbError>;

#[allow(clippy::unnecessary_wraps)]
pub fn apply<T, F>(f: F) -> JobResult<T>
where
    F: FnOnce(&mut T) + Send + 'static,
{
    Ok(Box::new(f))
}

pub struct Jobs<T> {
    tx: Sender<JobResult<T>>,
    rx: Receiver<JobResult<T>>,
    ctx: egui::Context,
    active: Arc<AtomicUsize>,
}

impl<T: 'static> Jobs<T> {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            tx,
            rx,
            ctx,
            active: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn spawn<F>(&self, f: F)
    where
        F: FnOnce() -> JobResult<T> + Send + 'static,
    {
        self.active.fetch_add(1, Ordering::SeqCst);
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        let active = Arc::clone(&self.active);

        thread::spawn(move || {
            let res = f();
            let _ = tx.send(res);
            active.fetch_sub(1, Ordering::SeqCst);
            ctx.request_repaint();
        });
    }

    pub fn drain(&self) -> Vec<JobResult<T>> {
        let mut results = Vec::new();
        while let Ok(res) = self.rx.try_recv() {
            results.push(res);
        }
        results
    }

    pub fn busy(&self) -> bool {
        self.active.load(Ordering::Relaxed) > 0
    }
}
