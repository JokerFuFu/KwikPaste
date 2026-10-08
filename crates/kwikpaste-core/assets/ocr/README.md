# Bundled offline Windows OCR

Run `./scripts/prepare-windows-ocr.ps1 -Architecture x64` (or `arm64`) before
building the native workspace on Windows. The script exports an explicit Cargo
target and static-CRT target flags, and prepares pinned static Tesseract libraries.
It does not create a root `.cargo/config.toml` or change the 1.x Tauri workspace.
Generated assets are ignored; the installed app never downloads models. Native
dependencies live in `RUNNER_TEMP/kwikpaste-windows-ocr` in CI, or the system
temporary directory locally. Keep them outside Cargo `target`: Rust cache
pruning removes nested Git metadata and native library files there.

English and Simplified Chinese `tessdata_fast` models are pinned to revision
`87416418657359cb625c412a48b6e1d6d41c29bd`, SHA-256-verified at preparation and
again before native initialization. They are compiled into the executable,
so installer, portable, and executable-only portable updates all retain OCR.
Native libraries use vcpkg revision `9e3427bc82738568947beb508e78231f99c04f4c`.
No OCR DLL, PATH lookup, external model directory or Windows language-pack
installation is needed at runtime.

`OCR-NOTICES.txt` combines the pinned model license and all vcpkg library notices.
It is embedded in the executable, included in native Windows installer/portable
packages, and exposed best-effort in the configured `ocr-components` directory.
The model license SHA-256 is
`cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30`.

The Windows installer budget is 14 MiB warning / 16 MiB hard limit for the bundled
engine and models. CI run 37804934395 measured x64 at 10.93 MiB and ARM64 at
10.02 MiB for commit cb2d9e5; remeasure after changes to the engine or models.

macOS uses local Apple Vision with `zh-Hans` and `en-US`; no assets are required.
Both engines accept only a bounded immutable image snapshot: 20 MiB, 40 MP,
and 100,000 Unicode output characters. Call configuration before starting the
clipboard watcher. Windows reports readiness only after native initialization.
