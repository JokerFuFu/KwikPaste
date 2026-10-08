# Offline image OCR development

OCR belongs to the native 2.x workspace. The 1.x Tauri workspace is unchanged.

## Build and test

macOS uses Apple Vision. Build the native workspace with the repository's Rust toolchain:

```sh
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo build --locked -p kwikpaste-app
```

On macOS 27, the dependency profile's `strip=debuginfo` can produce a proc-macro dylib that dyld rejects with `mis-aligned LINKEDIT string pool`. For local development, explicitly override rustc stripping without changing published build profiles:

```sh
RUSTFLAGS='-C strip=none' cargo test --locked --workspace
```

Windows builds require the static OCR libraries and pinned models. Run preparation and Cargo in the same PowerShell session:

```powershell
./scripts/prepare-windows-ocr.ps1 -Architecture x64
cargo test --locked --workspace
cargo build --locked --release -p kwikpaste-app --features production-identity --target x86_64-pc-windows-msvc
```

For ARM64, choose `-Architecture arm64` and `--target aarch64-pc-windows-msvc`. Preparation verifies models and licenses, sets the target/static-CRT environment, and creates ignored generated assets. Model bytes and notices are embedded in the executable. Installer and portable packaging also deliver `OCR-NOTICES.txt`. See [component details](../crates/kwikpaste-core/assets/ocr/README.md).

## Data and behavior

- OCR is disabled by default. Enabling it queues new images; indexing existing images is explicit. Failed jobs can be retried through the historical indexing action.
- Pause stops recognition but preserves completed search matches. Disable excludes OCR matches immediately. Clear pauses the worker and clears only derived rows.
- Migration 0008 supports both clean KwikPaste databases and the retained `image_ocr` table from an EcoPaste migration. Published migrations 0001..0007 remain unchanged.
- OCR search uses the existing history filters and matches literals correctly. Index changes clear bulk selection, while stale asynchronous select-all responses cannot restore invisible targets.
- Recognition is serial and off the UI/capture thread. Bounded immutable inputs and job tokens/generations prevent late native results from reappearing after clear, policy changes, storage replacement, or shutdown.

Use development/selftest identities and synthetic data for UI validation. Never point a development build at the only production database. A stock 2.0.0 application cannot reopen a database upgraded to migration 0008; preserve a cold backup and matching old executable before installing a test build. Until the fork has its own compatible update channel, avoid replacing an OCR build with the stock upstream updater.
