//! Disposable local image text and a serial background queue, separate from clipboard history.

pub mod native;

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde::Serialize;
use tokio::task::JoinHandle;

use crate::db::ocr as repository;
use crate::error::Result;
use crate::events::CoreEvent;
use crate::root::{Core, CoreInner};
use crate::settings::SettingsDelta;

/// Serializes queue publication with policy edits, clear and data replacement.
#[derive(Default)]
pub(crate) struct OcrRuntime {
    pub(crate) gate: tokio::sync::Mutex<()>,
    generation: AtomicU64,
    stopped: AtomicBool,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl OcrRuntime {
    pub(crate) fn invalidate(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageOcrStatus {
    pub supported: bool,
    pub enabled: bool,
    pub paused: bool,
    pub total: i64,
    pub pending: i64,
    pub completed: i64,
    pub failed: i64,
}

impl Core {
    pub async fn image_ocr_status(&self) -> Result<ImageOcrStatus> {
        let core = self.clone();
        self.hop(async move { status(&core.0).await }).await
    }

    /// Enqueue missing images and explicitly retry failures, retaining completed attempts.
    pub async fn queue_image_ocr_history(&self) -> Result<ImageOcrStatus> {
        let core = self.clone();
        self.hop(async move {
            let _gate = core.0.ocr.gate.lock().await;
            if core.0.ocr.stopped.load(Ordering::SeqCst)
                || !native::supported()
                || !core.settings().clipboard.ocr.enabled
            {
                return Err(anyhow::anyhow!("Image OCR is not enabled or supported").into());
            }
            repository::enqueue_history(&core.0.db.pool().await).await?;
            core.0.events.emit(CoreEvent::ImageOcrChanged);
            status(&core.0).await
        })
        .await
    }

    /// Pause and invalidate in-flight recognition before deleting only the derived index.
    pub async fn clear_image_ocr(&self) -> Result<ImageOcrStatus> {
        let core = self.clone();
        self.hop(async move {
            let _gate = core.0.ocr.gate.lock().await;
            core.0.ocr.invalidate();
            let patch = serde_json::json!({"clipboard":{"ocr":{"paused":true}}});
            let delta = SettingsDelta::from_patch(&patch);
            let next = core.0.settings.update(patch)?;
            core.0.events.emit(CoreEvent::SettingsUpdated {
                settings: Arc::new(next),
                delta,
            });
            repository::clear(&core.0.db.pool().await).await?;
            core.0.events.emit(CoreEvent::ImageOcrChanged);
            status(&core.0).await
        })
        .await
    }

    /// Configure local engine resources before starting clipboard capture.
    pub fn configure_image_ocr_models(&self, model_dir: &Path) -> Result<()> {
        native::configure(model_dir)?;
        self.0.events.emit(CoreEvent::ImageOcrChanged);
        Ok(())
    }
}

async fn status(core: &CoreInner) -> Result<ImageOcrStatus> {
    let policy = core.settings.snapshot().clipboard.ocr;
    let counts = repository::counts(&core.db.pool().await).await?;
    Ok(ImageOcrStatus {
        supported: native::supported(),
        enabled: policy.enabled,
        paused: policy.paused,
        total: counts.total,
        pending: counts.pending,
        completed: counts.completed,
        failed: counts.failed,
    })
}

/// Captures queue metadata only; recognition never runs on the capture path.
pub(crate) async fn on_capture(core: &CoreInner, pool: &sqlx::SqlitePool, id: &str) -> Result<()> {
    let _gate = core.ocr.gate.lock().await;
    if !core.ocr.stopped.load(Ordering::SeqCst)
        && native::supported()
        && core.settings.snapshot().clipboard.ocr.enabled
    {
        repository::enqueue(pool, id).await?;
        core.events.emit(CoreEvent::ImageOcrChanged);
    }
    Ok(())
}

pub(crate) fn settings_changed(core: &CoreInner) {
    core.ocr.invalidate();
    core.events.emit(CoreEvent::ImageOcrChanged);
}

/// Keep only a weak root between steps so dropping Core releases background work.
pub(crate) fn spawn(core: &Arc<CoreInner>) {
    let weak = Arc::downgrade(core);
    let task = core.rt.spawn(async move {
        loop {
            if weak
                .upgrade()
                .is_none_or(|core| core.ocr.stopped.load(Ordering::SeqCst))
            {
                break;
            }
            if let Err(err) = step_with(weak.clone(), native::recognize).await {
                log::warn!("image OCR queue step failed: {err}");
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
    *core
        .ocr
        .worker
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(task);
}

/// Native work may finish after shutdown, but its aborted publisher cannot write or emit.
pub(crate) async fn shutdown(core: &CoreInner) {
    let _gate = core.ocr.gate.lock().await;
    core.ocr.stopped.store(true, Ordering::SeqCst);
    core.ocr.invalidate();
    let task = core
        .ocr
        .worker
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    if let Some(task) = task {
        task.abort();
        let _ = task.await;
    }
}

/// Narrow synchronous recognition seam lets tests hold a real in-flight job across policy edits.
async fn step_with<F>(weak: Weak<CoreInner>, recognize: F) -> Result<()>
where
    F: FnOnce(&Path) -> anyhow::Result<String> + Send + 'static,
{
    let (pool, job, path, generation, runtime) = {
        let Some(core) = weak.upgrade() else {
            return Ok(());
        };
        let _gate = core.ocr.gate.lock().await;
        let policy = core.settings.snapshot().clipboard.ocr;
        if core.ocr.stopped.load(Ordering::SeqCst)
            || !native::supported()
            || !policy.enabled
            || policy.paused
            || core.watcher_pause.is_paused()
        {
            return Ok(());
        }
        let pool = core.db.pool().await;
        let Some(job) = repository::next_job(&pool).await? else {
            return Ok(());
        };
        if crate::clipboard::validate_image_file_name(&job.content).is_err() {
            repository::finish(&pool, &job, None).await?;
            core.events.emit(CoreEvent::ImageOcrChanged);
            return Ok(());
        }
        let path = core.images.origin_path(&job.content);
        (pool, job, path, core.ocr.generation(), core.rt.clone())
    };
    let source = path.clone();
    let recognized = runtime.spawn_blocking(move || recognize(&path)).await;
    let Some(core) = weak.upgrade() else {
        return Ok(());
    };
    let _gate = core.ocr.gate.lock().await;
    let policy = core.settings.snapshot().clipboard.ocr;
    if core.ocr.stopped.load(Ordering::SeqCst)
        || !policy.enabled
        || policy.paused
        || generation != core.ocr.generation()
        || pool.is_closed()
        || core.watcher_pause.is_paused()
        || source != core.images.origin_path(&job.content)
    {
        return Ok(());
    }
    let text = match recognized {
        Ok(Ok(text)) => Some(text),
        _ => None,
    };
    if repository::finish(&pool, &job, text.as_deref()).await? {
        core.events.emit(CoreEvent::ImageOcrChanged);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::{MemoryClipboard, MemoryState};
    use crate::db::models::ClipboardItemQuery;
    use crate::testing::{block_on, sample_png, Fixture};

    fn queued_image(core: &Core) -> String {
        let image = MemoryClipboard::with_state(MemoryState {
            png: Some(sample_png(16, 16)),
            ..Default::default()
        });
        let item = core
            .build_item(&core.read_payload(&image).unwrap().unwrap())
            .unwrap()
            .unwrap();
        let id = block_on(core.store_item(item, None)).unwrap().id;
        id
    }

    fn stop_automatic_worker(core: &Core) {
        if let Some(task) = core.0.ocr.worker.lock().unwrap().take() {
            task.abort();
        }
    }

    #[test]
    fn image_ocr_capture_backfill_clear_and_reopen_preserve_history() {
        let fixture = Fixture::new();
        let core = fixture.start();
        core.configure_image_ocr_models(&fixture.root().join("ocr-components"))
            .unwrap();
        stop_automatic_worker(&core);
        let initial = block_on(core.image_ocr_status()).unwrap();
        assert!(!initial.enabled);
        assert_eq!(initial.total, 0);
        block_on(core.update_settings(
            serde_json::json!({"clipboard":{"ocr":{"enabled":true,"paused":true}}}),
        ))
        .unwrap();
        let id = queued_image(&core);
        assert_eq!(block_on(core.image_ocr_status()).unwrap().pending, 1);
        assert_eq!(block_on(core.queue_image_ocr_history()).unwrap().pending, 1);
        block_on(core.shutdown()).unwrap();
        let reopened = fixture.start();
        reopened
            .configure_image_ocr_models(&fixture.root().join("ocr-components"))
            .unwrap();
        stop_automatic_worker(&reopened);
        assert_eq!(block_on(reopened.image_ocr_status()).unwrap().pending, 1);
        assert!(reopened.settings().clipboard.ocr.enabled);
        assert!(reopened.settings().clipboard.ocr.paused);
        let before = block_on(reopened.find_item(&id)).unwrap().unwrap();
        let bytes = std::fs::read(reopened.image_origin_path(&before.content).unwrap()).unwrap();
        let cleared = block_on(reopened.clear_image_ocr()).unwrap();
        assert!(cleared.paused);
        assert_eq!(cleared.total, 0);
        let after = block_on(reopened.find_item(&id)).unwrap().unwrap();
        assert_eq!(
            serde_json::to_value(before).unwrap(),
            serde_json::to_value(after.clone()).unwrap()
        );
        assert_eq!(
            bytes,
            std::fs::read(reopened.image_origin_path(&after.content).unwrap()).unwrap()
        );
        assert!(fixture
            .take_events()
            .iter()
            .any(|event| matches!(event, CoreEvent::ImageOcrChanged)));
        block_on(reopened.shutdown()).unwrap();
    }

    #[test]
    fn image_ocr_serial_worker_rejects_results_after_clear_disable_switch_or_shutdown() {
        for change in ["clear", "disable", "pause", "switch", "shutdown"] {
            let fixture = Fixture::new();
            let core = fixture.start();
            core.configure_image_ocr_models(&fixture.root().join("ocr-components"))
                .unwrap();
            stop_automatic_worker(&core);
            block_on(
                core.update_settings(serde_json::json!({"clipboard":{"ocr":{"enabled":true}}})),
            )
            .unwrap();
            queued_image(&core);
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let weak = Arc::downgrade(&core.0);
            let worker = core.runtime().spawn(step_with(weak, move |_| {
                started_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                Ok("stale invoice".into())
            }));
            started_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            match change {
                "clear" => {
                    block_on(core.clear_image_ocr()).unwrap();
                }
                "disable" => {
                    block_on(core.update_settings(
                        serde_json::json!({"clipboard":{"ocr":{"enabled":false}}}),
                    ))
                    .unwrap();
                    block_on(core.update_settings(
                        serde_json::json!({"clipboard":{"ocr":{"enabled":true}}}),
                    ))
                    .unwrap();
                }
                "pause" => {
                    block_on(
                        core.update_settings(
                            serde_json::json!({"clipboard":{"ocr":{"paused":true}}}),
                        ),
                    )
                    .unwrap();
                    block_on(core.update_settings(
                        serde_json::json!({"clipboard":{"ocr":{"paused":false}}}),
                    ))
                    .unwrap();
                }
                "switch" => {
                    block_on(core.change_storage_location(fixture.root().join("new-location")))
                        .unwrap();
                }
                "shutdown" => {
                    block_on(core.shutdown()).unwrap();
                }
                _ => unreachable!(),
            }
            fixture.take_events();
            release_tx.send(()).unwrap();
            block_on(worker).unwrap().unwrap();
            assert!(
                !fixture
                    .take_events()
                    .iter()
                    .any(|event| matches!(event, CoreEvent::ImageOcrChanged)),
                "late event after {change}"
            );
            if change == "shutdown" {
                let reopened = fixture.start();
                reopened
                    .configure_image_ocr_models(&fixture.root().join("ocr-components"))
                    .unwrap();
                stop_automatic_worker(&reopened);
                assert_eq!(block_on(reopened.image_ocr_status()).unwrap().completed, 0);
                block_on(reopened.shutdown()).unwrap();
            } else {
                assert_eq!(
                    block_on(core.image_ocr_status()).unwrap().completed,
                    0,
                    "late result after {change}"
                );
                let query = ClipboardItemQuery {
                    keyword: Some("stale invoice".into()),
                    ..Default::default()
                };
                assert_eq!(block_on(core.query_items_raw(query)).unwrap().1, 0);
                block_on(core.shutdown()).unwrap();
            }
        }
    }

    #[test]
    fn image_ocr_success_and_failed_attempts_emit_status_and_do_not_loop() {
        let fixture = Fixture::new();
        let core = fixture.start();
        core.configure_image_ocr_models(&fixture.root().join("ocr-components"))
            .unwrap();
        stop_automatic_worker(&core);
        block_on(core.update_settings(serde_json::json!({"clipboard":{"ocr":{"enabled":true}}})))
            .unwrap();
        queued_image(&core);
        block_on(
            core.runtime()
                .spawn(step_with(Arc::downgrade(&core.0), |_| {
                    Err(anyhow::anyhow!("fixture failure"))
                })),
        )
        .unwrap()
        .unwrap();
        assert_eq!(block_on(core.image_ocr_status()).unwrap().failed, 1);
        block_on(
            core.runtime()
                .spawn(step_with(Arc::downgrade(&core.0), |_| {
                    panic!("failed jobs must not be retried automatically")
                })),
        )
        .unwrap()
        .unwrap();
        assert_eq!(block_on(core.queue_image_ocr_history()).unwrap().pending, 1);
        block_on(
            core.runtime()
                .spawn(step_with(Arc::downgrade(&core.0), |_| {
                    Ok("本地 invoice".into())
                })),
        )
        .unwrap()
        .unwrap();
        assert_eq!(block_on(core.image_ocr_status()).unwrap().completed, 1);
        assert_eq!(
            block_on(core.query_items_raw(ClipboardItemQuery {
                keyword: Some("本地".into()),
                ..Default::default()
            }))
            .unwrap()
            .1,
            1
        );
        block_on(core.shutdown()).unwrap();
    }
}
