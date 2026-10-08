pub mod alignment;
pub mod entry;
pub mod inspect;
pub mod lyrics;
pub mod media;
pub mod model;
pub mod music;
mod music_import;
pub mod ocr;
pub mod presentation;
pub mod quality;
pub mod revision;
pub mod roformer;
pub mod runtime;
pub mod spectral;
pub mod suno;
pub mod web;

#[cfg(test)]
#[path = "../integrations/admin-console/music_bridge.rs"]
mod admin_console_bridge;

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Default)]
pub struct Progress {
    pub message: Arc<Mutex<String>>,
    pub cancelled: Arc<AtomicBool>,
}

impl Progress {
    pub fn set(&self, message: impl Into<String>) {
        *self.message.lock().unwrap_or_else(|e| e.into_inner()) = message.into();
    }
    pub fn get(&self) -> String {
        self.message
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn check(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.cancelled.load(Ordering::Relaxed),
            "処理を取り消しました。"
        );
        Ok(())
    }
}
