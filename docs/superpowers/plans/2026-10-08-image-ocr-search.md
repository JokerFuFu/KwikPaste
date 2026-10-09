# Native image OCR search implementation plan

> **For agentic workers:** Use subagent-driven-development to implement these tasks and review the integrated change.

**Goal:** Port the user's opt-in offline image OCR search to native KwikPaste on macOS and Windows, preserving existing history.

**Architecture:** Recognition and the durable serial queue live in kwikpaste-core; GPUI exposes settings and queue status. Search combines image OCR with existing text and note matching in the same filtered query. Native resources are configured before the clipboard watcher starts.

**Tech Stack:** Rust, SQLx/SQLite FTS5, Tokio, Apple Vision, statically linked Tesseract with pinned English and Simplified Chinese models, GPUI.

## Global constraints

- macOS and Windows only. No WebView or cloud OCR.
- User approved a separate feature branch in their fork; this overrides upstream's default main-only workflow.
- OCR defaults off for new settings; migrated settings retain the user's choice.
- Do not modify installed app or production clipboard data during implementation and validation.
- Published SQL migrations 0001..0007 stay byte-identical. Add 0008 to support both a clean KwikPaste database and the user's already-existing image_ocr tables.
- Preserve original clipboard rows, group metadata, notes, favorites, image bytes, and content hashes.
- Bound input to 20 MiB and 40 megapixels; bound output to 100,000 Unicode characters.
- Disabled OCR excludes derived matches immediately. Paused recognition keeps completed results searchable. Clearing derived data pauses the queue and leaves history intact.
- Queue new images automatically when enabled. Explicit history indexing also retries failures.
- Clear, disable, shutdown, and data-root replacement must prevent stale native results from being published.
- Use bilingual Chinese/English UI and existing semantic theme tokens.
- Windows assets and build dependencies must work with x64/ARM64 native release and portable packages; describe untested architecture/runtime boundaries honestly.

## Task 1: Core queue, migration and search

**Files:** `crates/kwikpaste-core/src/ocr/mod.rs`, `src/db/ocr.rs`, `src/db/items.rs`, `src/db/models.rs`, `src/root.rs`, `src/events.rs`, `src/settings/model.rs`, storage/backup integration and `migrations/0008_image_ocr.sql`.

**Interfaces produced:**

```rust
impl Core {
    pub async fn image_ocr_status(&self) -> Result<ImageOcrStatus>;
    pub async fn queue_image_ocr_history(&self) -> Result<ImageOcrStatus>;
    pub async fn clear_image_ocr(&self) -> Result<ImageOcrStatus>;
    pub fn configure_image_ocr_models(&self, model_dir: &std::path::Path) -> Result<()>;
}
// Public serializable/cloneable status fields:
// supported, enabled, paused: bool; total, pending, completed, failed: i64.
// Settings: clipboard.ocr.enabled / clipboard.ocr.paused.
// CoreEvent::ImageOcrChanged requests status and search refresh (no fake item id).
```

- [ ] Add failing integration tests demonstrating OCR-only matching is absent, then add the switch-aware filtered query.
- [ ] Test one- and two-character Chinese terms, longer FTS terms, literal punctuation, note search while disabled, filtering/count consistency, and no duplicate image rows.
- [ ] Test migration from normal 0007 database and 0007 database containing legacy OCR tables and populated derived text. Verify original columns and metadata remain unchanged.
- [ ] Port repository functions and serial worker from existing EcoPaste OCR, replacing Tauri handles with weak CoreInner and CoreRuntime.
- [ ] Test clear/disable/data switch during recognition and orderly worker shutdown. Keep native recognition behind a narrow synchronous function seam.
- [ ] Run targeted core tests and then full core tests.

## Task 2: Native engines and Windows packaging

**Files:** `crates/kwikpaste-core/src/ocr/native.rs`, `windows.rs`, fixture images, core `Cargo.toml` and `build.rs`, Windows preparation scripts, native CI/release workflows and native packaging.

**Interfaces produced:**

```rust
pub fn supported() -> bool;
pub fn configure(model_dir: &std::path::Path) -> anyhow::Result<()>;
pub fn recognize(path: &std::path::Path) -> anyhow::Result<String>;
```

- [ ] Reuse existing validated Vision and Tesseract adapters and tests; keep Unicode paths and bounded snapshot reads.
- [ ] Configure native components before Core starts capturing. Windows readiness requires validated pinned models and native initialization.
- [ ] Adapt static CRT/Tesseract preparation to this workspace without creating root `.cargo/config.toml`.
- [ ] Ensure installed and portable packages include the models and license notices, or embed models with equivalent verification and license delivery.
- [ ] Run macOS native tests using Chinese/English fixture images; Windows CI must compile and run the corresponding real engine tests.

## Task 3: GPUI settings and event refresh

**Files:** `crates/kwikpaste-app/src/preferences/schema.rs`, `view.rs` and focused OCR view module; `core_host.rs`; clipboard source/view event handling; both preferences locale files.

**Interfaces consumed:** Core methods, settings paths and `CoreEvent::ImageOcrChanged` above.

- [ ] Add default-off OCR toggle, pause/resume, index historical images/retry, clear derived index, and counts/readiness status.
- [ ] Use async Core calls, never recognition or database work in render callbacks.
- [ ] Refresh status and filtered list after OCR completion, clear, or enable changes; hidden window lifecycle must not retain stale results.
- [ ] Add localized clear confirmation explaining original images/history are preserved.
- [ ] Run formatting, app checks and actual GPUI interaction in a development identity with synthetic data.

## Integration and delivery

- [ ] Review task boundaries and all changes for data safety and cross-platform build correctness.
- [ ] Validate adopted legacy table compatibility on a copied database; never migrate the live production database during testing.
- [ ] Run workspace Clippy/tests and Windows CI. Record actual tested architectures and runtime limitations.
- [ ] Update user-facing README/changelog and developer build guidance.
- [ ] Commit and push the feature branch to JokerFuFu/KwikPaste; prepare a reviewable fork PR and downloadable test build. Production replacement remains a separate final step after verification.
