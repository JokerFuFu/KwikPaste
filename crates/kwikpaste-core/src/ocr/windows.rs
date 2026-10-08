//! Bundled, offline Tesseract. The only filesystem reads use Rust paths; FFI receives bytes.

use std::ffi::{c_char, c_int, CStr};
use std::io::Cursor;
use std::path::Path;
use std::ptr::NonNull;
use std::sync::OnceLock;

use anyhow::{ensure, Context};
use sha2::{Digest, Sha256};

use super::{bounded_text, read_image, MAX_OUTPUT_CHARS, MAX_PIXELS};

const MAX_MODEL_BYTES: u64 = 16 * 1024 * 1024;
const OEM_LSTM_ONLY: c_int = 1;
const PSM_AUTO: c_int = 3;

struct ModelSpec {
    language: &'static CStr,
    file_name: &'static str,
    sha256: &'static str,
}

// tessdata_fast 87416418657359cb625c412a48b6e1d6d41c29bd, Apache-2.0.
// Reject unrecognized bytes before Init5: an invalid memory buffer can fall back to a disk lookup.
const MODEL_SPECS: [ModelSpec; 2] = [
    ModelSpec {
        language: c"eng",
        file_name: "eng.traineddata",
        sha256: "7d4322bd2a7749724879683fc3912cb542f19906c83bcc1a52132556427170b2",
    },
    ModelSpec {
        language: c"chi_sim",
        file_name: "chi_sim.traineddata",
        sha256: "a5fcb6f0db1e1d6d8522f39db4e848f05984669172e584e8d76b6b3141e1f730",
    },
];

struct Models {
    bytes: [&'static [u8]; 2],
}

#[derive(Default)]
struct Provider {
    models: OnceLock<Models>,
}

static PROVIDER: Provider = Provider {
    models: OnceLock::new(),
};

/// Initializes embedded models and exposes notices in the writable data directory.
/// Models travel inside the exe, including portable updates that replace only that file.
pub fn configure(model_dir: &Path) -> anyhow::Result<()> {
    PROVIDER.configure()?;
    if let Err(error) = write_notices(model_dir) {
        log::warn!("Cannot expose bundled OCR notices: {error:#}");
    }
    Ok(())
}

fn write_notices(directory: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(directory).context("Cannot create OCR notice directory")?;
    let notice_path = directory.join("OCR-NOTICES.txt");
    if std::fs::read(&notice_path).ok().as_deref() != Some(NOTICES.as_bytes()) {
        std::fs::write(&notice_path, NOTICES).context("Cannot write bundled OCR notices")?;
    }
    Ok(())
}

const NOTICES: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/ocr/OCR-NOTICES.txt"
));
const EMBEDDED_MODELS: [&[u8]; 2] = [
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/ocr/tessdata/eng.traineddata"
    )),
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/ocr/tessdata/chi_sim.traineddata"
    )),
];

pub fn supported() -> bool {
    PROVIDER.supported()
}

/// Called only by the serial blocking OCR worker; each native handle stays on its creating thread.
pub fn recognize(path: &Path) -> anyhow::Result<String> {
    PROVIDER.recognize(path)
}

impl Provider {
    fn supported(&self) -> bool {
        self.models.get().is_some()
    }

    /// Publish only after both pinned models initialize; failures leave readiness false.
    fn configure(&self) -> anyhow::Result<()> {
        if self.supported() {
            return Ok(());
        }
        let models = Models {
            bytes: EMBEDDED_MODELS,
        };
        for (spec, bytes) in MODEL_SPECS.iter().zip(&models.bytes) {
            validate_model(bytes, spec)?;
            NativeApi::new(bytes, spec.language)?;
        }
        // Concurrent setup calls can validate identical pinned models; the first completed snapshot wins.
        let _ = self.models.set(models);
        Ok(())
    }

    /// Validate and decode once, then run English and Chinese independently against the same pixels.
    fn recognize(&self, path: &Path) -> anyhow::Result<String> {
        let models = self
            .models
            .get()
            .context("Bundled Windows OCR models are not ready")?;
        let (bytes, width, height) = read_image(path)?;
        let mut reader = image::ImageReader::new(Cursor::new(&bytes))
            .with_guessed_format()
            .context("Invalid OCR image format")?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(width);
        limits.max_image_height = Some(height);
        limits.max_alloc = Some(MAX_PIXELS * 8);
        reader.limits(limits);
        let decoded = reader
            .decode()
            .context("Cannot decode OCR image")?
            .into_rgba8();
        let mut rgb = Vec::with_capacity((u64::from(width) * u64::from(height) * 3) as usize);
        // Transparent clipboard PNGs should be recognized as they appear on a white page.
        for pixel in decoded.pixels() {
            let alpha = u16::from(pixel[3]);
            for color in &pixel.0[..3] {
                rgb.push(((u16::from(*color) * alpha + 255 * (255 - alpha) + 127) / 255) as u8);
            }
        }
        drop(decoded);
        let mut recognized = Vec::with_capacity(MODEL_SPECS.len());
        for (spec, data) in MODEL_SPECS.iter().zip(&models.bytes) {
            let api = NativeApi::new(data, spec.language)?;
            recognized.push(api.recognize(&rgb, width, height)?);
        }
        Ok(bounded_text(recognized))
    }
}

/// Verify release-pinned bytes before Init5, which can otherwise fall back to a disk lookup.
fn validate_model(bytes: &[u8], spec: &ModelSpec) -> anyhow::Result<()> {
    ensure!(
        bytes.len() as u64 <= MAX_MODEL_BYTES,
        "Bundled OCR model exceeds 16 MiB"
    );
    ensure!(
        format!("{:x}", Sha256::digest(bytes)) == spec.sha256,
        "Bundled OCR model {} checksum does not match this release",
        spec.file_name
    );
    Ok(())
}

/// Removes only horizontal spacing between neighboring Han ideographs before applying the output limit.
/// English, mixed-language, punctuation and line boundaries retain their original separators.
fn normalize_cjk_spacing(text: &str) -> String {
    let mut characters = text.chars().peekable();
    let mut previous_han = false;
    std::iter::from_fn(move || loop {
        let character = characters.next()?;
        if matches!(character, ' ' | '\t') && previous_han {
            let mut next = characters.clone();
            while next
                .peek()
                .is_some_and(|character| matches!(character, ' ' | '\t'))
            {
                next.next();
            }
            if next.peek().is_some_and(|character| is_han(*character)) {
                characters = next;
                continue;
            }
        }
        previous_han = is_han(character);
        return Some(character);
    })
    .take(MAX_OUTPUT_CHARS)
    .collect()
}

fn is_han(character: char) -> bool {
    matches!(character, '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}' | '\u{f900}'..='\u{faff}')
}

#[repr(C)]
struct TessBaseApi {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn TessBaseAPICreate() -> *mut TessBaseApi;
    fn TessBaseAPIDelete(handle: *mut TessBaseApi);
    fn TessBaseAPIInit5(
        handle: *mut TessBaseApi,
        data: *const c_char,
        data_size: c_int,
        language: *const c_char,
        mode: c_int,
        configs: *mut *mut c_char,
        configs_size: c_int,
        vars_vec: *mut *mut c_char,
        vars_values: *mut *mut c_char,
        vars_vec_size: usize,
        set_only_non_debug_params: c_int,
    ) -> c_int;
    fn TessBaseAPISetPageSegMode(handle: *mut TessBaseApi, mode: c_int);
    fn TessBaseAPISetImage(
        handle: *mut TessBaseApi,
        image: *const u8,
        width: c_int,
        height: c_int,
        bytes_per_pixel: c_int,
        bytes_per_line: c_int,
    );
    fn TessBaseAPISetSourceResolution(handle: *mut TessBaseApi, ppi: c_int);
    fn TessBaseAPIGetUTF8Text(handle: *mut TessBaseApi) -> *mut c_char;
    fn TessDeleteText(text: *mut c_char);
}

struct NativeApi(NonNull<TessBaseApi>);

impl NativeApi {
    /// Tesseract 5 capi.cpp forwards Init5 to BaseAPI::Init(data, size), whose TessdataManager::LoadMemBuffer
    /// consumes one model. Two languages need two independent calls, not "eng+chi_sim" on one buffer.
    /// https://github.com/tesseract-ocr/tesseract/blob/5.5.0/src/api/baseapi.cpp
    fn new(model: &[u8], language: &CStr) -> anyhow::Result<Self> {
        // SAFETY: Create returns a new opaque owned handle, released by Drop on every path.
        let api = Self(
            NonNull::new(unsafe { TessBaseAPICreate() })
                .context("Cannot allocate local OCR engine")?,
        );
        let mut names = [c"tessedit_load_sublangs".as_ptr().cast_mut()];
        let mut values = [c"".as_ptr().cast_mut()];
        // SAFETY: The hash-validated model is nonempty and bounded below i32::MAX; all pointers
        // remain valid for Init5, which copies data. Disabling sublanguages prevents disk fallbacks.
        let result = unsafe {
            TessBaseAPIInit5(
                api.0.as_ptr(),
                model.as_ptr().cast(),
                model.len() as c_int,
                language.as_ptr(),
                OEM_LSTM_ONLY,
                std::ptr::null_mut(),
                0,
                names.as_mut_ptr(),
                values.as_mut_ptr(),
                names.len(),
                0,
            )
        };
        ensure!(result == 0, "Bundled local OCR model could not initialize");
        // SAFETY: The handle is initialized; AUTO does not require an external OSD model.
        unsafe { TessBaseAPISetPageSegMode(api.0.as_ptr(), PSM_AUTO) };
        Ok(api)
    }

    fn recognize(&self, image: &[u8], width: u32, height: u32) -> anyhow::Result<String> {
        // SAFETY: The decoded RGB buffer has width*height*3 bytes; dimensions were capped at 40 MP.
        // SetImage copies the pixels and text is owned by TessDeleteText, including on UTF-8 errors.
        let text = unsafe {
            TessBaseAPISetImage(
                self.0.as_ptr(),
                image.as_ptr(),
                width as c_int,
                height as c_int,
                3,
                (width * 3) as c_int,
            );
            TessBaseAPISetSourceResolution(self.0.as_ptr(), 300);
            TessBaseAPIGetUTF8Text(self.0.as_ptr())
        };
        let owned_text = NativeText(NonNull::new(text).context("Local OCR recognition failed")?);
        // SAFETY: GetUTF8Text returns a NUL-terminated allocation which lives until NativeText drops.
        let text = unsafe { CStr::from_ptr(owned_text.0.as_ptr()) }
            .to_str()
            .context("Local OCR returned invalid UTF-8")?;
        Ok(bounded_text(text.lines().map(normalize_cjk_spacing)))
    }
}

impl Drop for NativeApi {
    fn drop(&mut self) {
        // SAFETY: This handle was created by TessBaseAPICreate and has exactly one owner.
        unsafe { TessBaseAPIDelete(self.0.as_ptr()) };
    }
}

struct NativeText(NonNull<c_char>);

impl Drop for NativeText {
    fn drop(&mut self) {
        // SAFETY: This allocation was returned by GetUTF8Text and has exactly one owner.
        unsafe { TessDeleteText(self.0.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/ocr/native_fixtures")
            .join(name)
    }

    /// CI must prepare the pinned resources; missing models are a test failure, not a skip.
    fn configured_provider() -> Provider {
        let provider = Provider::default();
        provider
            .configure()
            .expect("Prepare bundled OCR models before running Windows OCR tests");
        assert!(provider.supported());
        provider
    }

    #[test]
    fn recognizes_bundled_english_fixture() {
        let result = configured_provider()
            .recognize(&fixture("english.png"))
            .unwrap();
        assert!(result.contains("EcoPaste local image search"));
    }

    #[test]
    fn recognizes_bundled_chinese_fixture() {
        let result = configured_provider()
            .recognize(&fixture("chinese.png"))
            .unwrap();
        assert!(result.contains("本地图片文字识别测试"));
    }

    #[test]
    fn recognizes_image_from_unicode_path_with_embedded_models() {
        let directory = tempfile::tempdir().unwrap();
        let image = directory.path().join("图片 空格 😀.png");
        std::fs::copy(fixture("chinese.png"), &image).unwrap();
        let result = configured_provider().recognize(&image).unwrap();
        assert!(result.contains("本地图片文字识别测试"));
    }

    #[test]
    fn unconfigured_provider_does_not_report_supported() {
        let provider = Provider::default();
        assert!(!provider.supported());
        assert!(provider
            .recognize(&fixture("english.png"))
            .unwrap_err()
            .to_string()
            .contains("not ready"));
    }

    #[test]
    fn corrupt_model_is_rejected_before_native_initialization() {
        assert!(validate_model(b"invalid model", &MODEL_SPECS[0])
            .unwrap_err()
            .to_string()
            .contains("checksum"));
    }

    #[test]
    fn oversized_model_is_rejected_before_native_initialization() {
        let bytes = vec![0; MAX_MODEL_BYTES as usize + 1];
        assert!(validate_model(&bytes, &MODEL_SPECS[0])
            .unwrap_err()
            .to_string()
            .contains("16 MiB"));
    }

    #[test]
    fn configure_exposes_notices_in_unicode_data_directory() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("模型 空格 😀");
        configure(&directory).unwrap();
        assert!(supported());
        assert_eq!(
            std::fs::read_to_string(directory.join("OCR-NOTICES.txt")).unwrap(),
            NOTICES
        );
    }

    #[test]
    fn unreadable_notice_directory_does_not_disable_embedded_engine() {
        let file = tempfile::NamedTempFile::new().unwrap();
        configure(file.path()).unwrap();
        assert!(supported());
    }

    #[test]
    fn rejects_truncated_image_before_recognition() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let bytes = std::fs::read(fixture("english.png")).unwrap();
        std::fs::write(file.path(), &bytes[..64]).unwrap();
        assert!(configured_provider().recognize(file.path()).is_err());
    }

    #[test]
    fn normalizes_only_horizontal_space_between_han_characters() {
        assert_eq!(normalize_cjk_spacing("本 地\t图  片"), "本地图片");
        assert_eq!(normalize_cjk_spacing("local image"), "local image");
        assert_eq!(normalize_cjk_spacing("本地 image 图片"), "本地 image 图片");
        assert_eq!(normalize_cjk_spacing("本 地 ， 图 片"), "本地 ， 图片");
        assert_eq!(normalize_cjk_spacing("本 地\n图 片"), "本地\n图片");
        assert_eq!(normalize_cjk_spacing("本 地\r\n图 片"), "本地\r\n图片");
        assert_eq!(normalize_cjk_spacing("本 地  "), "本地  ");
    }

    #[test]
    fn normalization_bounds_output_without_splitting_unicode() {
        let input = "文 ".repeat(MAX_OUTPUT_CHARS + 1);
        let normalized = normalize_cjk_spacing(&input);
        assert_eq!(normalized.chars().count(), MAX_OUTPUT_CHARS);
        assert!(normalized.chars().all(|character| character == '文'));
    }

    #[test]
    fn blank_image_has_no_text() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("blank.png");
        image::GrayImage::from_pixel(640, 480, image::Luma([255]))
            .save(&path)
            .unwrap();
        assert!(configured_provider().recognize(&path).unwrap().is_empty());
    }
}
