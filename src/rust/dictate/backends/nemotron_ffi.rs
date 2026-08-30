//! Small, stable-ABI loader for NVIDIA's NeMo-Speech.cpp ASR library.
//!
//! The official Windows/Linux archives expose `nemo_speech_asr_c` with an
//! append-only C ABI.  Loading that ABI dynamically keeps this crate free of
//! a CMake/CUDA build dependency while still running the recognizer in the
//! whisper-dictate process.  The opaque handles never cross this module's
//! public boundary.

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::ptr::{self, NonNull};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use libloading::Library;

type RawRecognizer = c_void;
type RawResult = c_void;

const STATUS_OK: i32 = 0;

#[repr(C)]
#[derive(Clone, Copy)]
struct BackendConfig {
    size: usize,
    gpu: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ModelConfig {
    size: usize,
    path: *const c_char,
    name: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RecognizerConfig {
    size: usize,
    backend: *const BackendConfig,
    model: *const ModelConfig,
    streaming: *const c_void,
    decoder: *const c_void,
    vad: *const c_void,
    endpointing: *const c_void,
    postproc: *const c_void,
    diar: *const c_void,
    batching: *const c_void,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SpeechContext {
    size: usize,
    phrases: *const *const c_char,
    phrase_count: usize,
    boost: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RecognitionOptions {
    size: usize,
    request_id: *const c_char,
    language_code: *const c_char,
    interim_results: bool,
    enable_word_time_offsets: bool,
    enable_automatic_punctuation: bool,
    verbatim_transcripts: bool,
    profanity_filter: bool,
    stop_history_eou_ms: i32,
    speech_contexts: *const SpeechContext,
    speech_context_count: usize,
    max_alternatives: i32,
    enable_speaker_diarization: bool,
    max_speaker_count: i32,
}

type CreateFn = unsafe extern "C" fn(*const RecognizerConfig, *mut *mut RawRecognizer) -> i32;
type DestroyFn = unsafe extern "C" fn(*mut RawRecognizer);
type OptionsDefaultFn = unsafe extern "C" fn() -> RecognitionOptions;
type RecognizeFn = unsafe extern "C" fn(
    *mut RawRecognizer,
    *const RecognitionOptions,
    *const f32,
    usize,
    i32,
    *mut *mut RawResult,
) -> i32;
type ResultTranscriptFn = unsafe extern "C" fn(*const RawResult, usize) -> *const c_char;
type ResultLanguageCountFn = unsafe extern "C" fn(*const RawResult, usize) -> usize;
type ResultLanguageCodeFn = unsafe extern "C" fn(*const RawResult, usize, usize) -> *const c_char;
type ResultDestroyFn = unsafe extern "C" fn(*mut RawResult);
type LastErrorFn = unsafe extern "C" fn() -> *const c_char;

struct NativeApi {
    // The library must outlive every opaque recognizer/result handle and every
    // function pointer copied below.
    _library: Library,
    create: CreateFn,
    destroy: DestroyFn,
    options_default: OptionsDefaultFn,
    recognize: RecognizeFn,
    result_transcript: ResultTranscriptFn,
    result_language_count: ResultLanguageCountFn,
    result_language_code: ResultLanguageCodeFn,
    result_destroy: ResultDestroyFn,
    last_error: LastErrorFn,
}

impl NativeApi {
    fn load(path: &Path) -> Result<Self> {
        let library = unsafe { load_native_library(path) }?;
        // `Library::get` borrows the library, so copy each function pointer
        // before moving the library into the API object.
        unsafe fn symbol<T: Copy>(library: &Library, name: &[u8]) -> Result<T> {
            Ok(*library
                .get::<T>(name)
                .with_context(|| format!("missing NeMo-Speech.cpp symbol {:?}", name))?)
        }
        Ok(Self {
            create: unsafe { symbol(&library, b"nemo_speech_asr_create\0")? },
            destroy: unsafe { symbol(&library, b"nemo_speech_asr_destroy\0")? },
            options_default: unsafe {
                symbol(&library, b"nemo_speech_asr_recognition_options_default\0")?
            },
            recognize: unsafe { symbol(&library, b"nemo_speech_asr_recognize_f32\0")? },
            result_transcript: unsafe { symbol(&library, b"nemo_speech_asr_result_transcript\0")? },
            result_language_count: unsafe {
                symbol(&library, b"nemo_speech_asr_result_language_count\0")?
            },
            result_language_code: unsafe {
                symbol(&library, b"nemo_speech_asr_result_language_code\0")?
            },
            result_destroy: unsafe { symbol(&library, b"nemo_speech_asr_result_destroy\0")? },
            last_error: unsafe { symbol(&library, b"nemo_speech_asr_last_error\0")? },
            _library: library,
        })
    }

    fn error(&self, status: i32) -> anyhow::Error {
        let detail = unsafe { (self.last_error)() };
        let detail = (!detail.is_null())
            .then(|| {
                unsafe { CStr::from_ptr(detail) }
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|message| !message.trim().is_empty())
            .unwrap_or_else(|| format!("status {status}"));
        anyhow!("NeMo-Speech.cpp ASR failed: {detail}")
    }
}

/// Load the native adapter without changing process-global DLL search state.
///
/// Windows needs the runtime archive's directory in the dependency search for
/// companion DLLs shipped beside `nemo_speech_asr_c.dll`. The generic
/// `Library::new` call does not provide that guarantee for an absolute path.
#[cfg(windows)]
unsafe fn load_native_library(path: &Path) -> Result<Library> {
    use libloading::os::windows::{
        Library as WindowsLibrary, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
    };

    // LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR requires a fully qualified path.
    let full_path = path
        .canonicalize()
        .with_context(|| format!("resolve NeMo-Speech.cpp ASR library {}", path.display()))?;
    let library = unsafe {
        WindowsLibrary::load_with_flags(
            &full_path,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        )
    }
    .with_context(|| format!("load NeMo-Speech.cpp ASR library {}", full_path.display()))?;
    Ok(library.into())
}

#[cfg(not(windows))]
unsafe fn load_native_library(path: &Path) -> Result<Library> {
    unsafe { Library::new(path) }
        .with_context(|| format!("load NeMo-Speech.cpp ASR library {}", path.display()))
}

/// A loaded Nemotron recognizer. The higher-level idle model serializes all
/// calls while the opaque handle is borrowed, so preview/final calls cannot
/// race one another. `Send` is the only auto-trait the generic idle wrapper
/// requires for the model value; it lets the wrapper own the handle on its
/// worker thread without exposing the raw pointer to callers.
pub(crate) struct NativeRecognizer {
    api: Arc<NativeApi>,
    raw: NonNull<RawRecognizer>,
}

// The C ABI owns the opaque handle's lifetime. It never crosses this module's
// public boundary; the idle wrapper's mutex serializes access before handing a
// reference to a transcription callback.
unsafe impl Send for NativeRecognizer {}

impl NativeRecognizer {
    pub(crate) fn new(library_path: &Path, model_path: &Path, gpu: i32) -> Result<Self> {
        let api = Arc::new(NativeApi::load(library_path)?);
        let model_path = CString::new(model_path.to_string_lossy().as_bytes())
            .context("Nemotron model path contains an embedded NUL")?;
        let backend = BackendConfig {
            size: std::mem::size_of::<BackendConfig>(),
            gpu,
        };
        let model = ModelConfig {
            size: std::mem::size_of::<ModelConfig>(),
            path: model_path.as_ptr(),
            name: ptr::null(),
        };
        let config = RecognizerConfig {
            size: std::mem::size_of::<RecognizerConfig>(),
            backend: &backend,
            model: &model,
            streaming: ptr::null(),
            decoder: ptr::null(),
            vad: ptr::null(),
            endpointing: ptr::null(),
            postproc: ptr::null(),
            diar: ptr::null(),
            batching: ptr::null(),
        };
        let mut raw = ptr::null_mut();
        let status = unsafe { (api.create)(&config, &mut raw) };
        if status != STATUS_OK {
            return Err(api.error(status));
        }
        let raw = NonNull::new(raw)
            .ok_or_else(|| anyhow!("NeMo-Speech.cpp returned a null recognizer"))?;
        Ok(Self { api, raw })
    }

    pub(crate) fn recognize(
        &self,
        samples: &[f32],
        sample_rate: u32,
        language: &str,
        prompt: Option<&str>,
        terms: &[String],
    ) -> Result<NativeResult> {
        let language =
            CString::new(language).context("Nemotron language contains an embedded NUL")?;
        // `initial_prompt_with_terms` folds the vocabulary into the Whisper
        // prompt, but NeMo speech contexts need each vocabulary item as its
        // own phrase. Prefer those bounded terms and use a prompt only when
        // the caller supplied no dictionary vocabulary.
        let phrase_values = speech_phrase_values(prompt, terms);
        let mut phrases = Vec::with_capacity(phrase_values.len());
        for value in phrase_values {
            phrases.push(
                CString::new(value).context("Nemotron speech phrase contains an embedded NUL")?,
            );
        }
        let phrase_ptrs = phrases
            .iter()
            .map(|phrase| phrase.as_ptr())
            .collect::<Vec<_>>();
        let speech_context = (!phrase_ptrs.is_empty()).then_some(SpeechContext {
            size: std::mem::size_of::<SpeechContext>(),
            phrases: phrase_ptrs.as_ptr(),
            phrase_count: phrase_ptrs.len(),
            boost: 10.0,
        });
        let mut options = unsafe { (self.api.options_default)() };
        options.size = std::mem::size_of::<RecognitionOptions>();
        options.language_code = language.as_ptr();
        options.enable_automatic_punctuation = true;
        if let Some(context) = speech_context.as_ref() {
            options.speech_contexts = context;
            options.speech_context_count = 1;
        }
        let mut result = ptr::null_mut();
        let status = unsafe {
            (self.api.recognize)(
                self.raw.as_ptr(),
                &options,
                samples.as_ptr(),
                samples.len(),
                i32::try_from(sample_rate).unwrap_or(i32::MAX),
                &mut result,
            )
        };
        if status != STATUS_OK {
            return Err(self.api.error(status));
        }
        let result = NonNull::new(result)
            .ok_or_else(|| anyhow!("NeMo-Speech.cpp returned a null recognition result"))?;
        let guard = ResultGuard {
            api: Arc::clone(&self.api),
            raw: result,
        };
        let text = unsafe { (self.api.result_transcript)(guard.raw.as_ptr(), 0) };
        let text = if !text.is_null() {
            unsafe { CStr::from_ptr(text) }
                .to_string_lossy()
                .into_owned()
        } else {
            String::new()
        };
        let language_count = unsafe { (self.api.result_language_count)(guard.raw.as_ptr(), 0) };
        let detected_language = (0..language_count).find_map(|index| {
            let value = unsafe { (self.api.result_language_code)(guard.raw.as_ptr(), 0, index) };
            (!value.is_null()).then(|| {
                unsafe { CStr::from_ptr(value) }
                    .to_string_lossy()
                    .into_owned()
            })
        });
        Ok(NativeResult {
            text,
            language: detected_language,
        })
    }
}

fn speech_phrase_values(prompt: Option<&str>, terms: &[String]) -> Vec<String> {
    let terms = terms
        .iter()
        .filter(|term| !term.trim().is_empty())
        .cloned()
        .collect::<Vec<_>>();
    let mut phrases = Vec::with_capacity(terms.len() + 1);
    if let Some(prompt) = prompt.filter(|value| !value.trim().is_empty()) {
        let vocabulary_suffix =
            (!terms.is_empty()).then(|| format!("Vocabulary: {}", terms.join(", ")));
        let prompt = prompt.trim();
        let base = match vocabulary_suffix.as_deref() {
            Some(suffix) if prompt == suffix => "",
            Some(suffix) => prompt
                .strip_suffix(&format!("\n{suffix}"))
                .unwrap_or(prompt)
                .trim(),
            None => prompt,
        };
        if !base.is_empty() {
            phrases.push(base.to_owned());
        }
    }
    phrases.extend(terms);
    phrases
}

#[cfg(all(test, any(windows, target_os = "linux")))]
#[path = "tests/nemotron_ffi_fixture.rs"]
mod nemotron_ffi_fixture;
#[cfg(test)]
#[path = "nemotron_ffi_tests.rs"]
mod nemotron_ffi_tests;
#[cfg(all(test, any(windows, target_os = "linux")))]
pub(crate) use nemotron_ffi_fixture::build_fixture_library;

impl Drop for NativeRecognizer {
    fn drop(&mut self) {
        unsafe { (self.api.destroy)(self.raw.as_ptr()) };
    }
}

struct ResultGuard {
    api: Arc<NativeApi>,
    raw: NonNull<RawResult>,
}

impl Drop for ResultGuard {
    fn drop(&mut self) {
        unsafe { (self.api.result_destroy)(self.raw.as_ptr()) };
    }
}

#[derive(Debug, Clone)]
pub(crate) struct NativeResult {
    pub(crate) text: String,
    pub(crate) language: Option<String>,
}

/// Locate the C ABI beside the executable before falling back to the platform
/// loader search path. An explicit path always wins, which is useful for a
/// portable install or a developer checkout of NeMo-Speech.cpp. Windows does
/// not accept a bare DLL name: the legacy loader search order includes the
/// current working directory, which would let an unrelated DLL be loaded.
pub(crate) fn resolve_library_path(explicit: Option<&str>) -> Result<PathBuf> {
    if let Some(value) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Ok(path);
        }
        return Err(anyhow!(
            "NeMo-Speech.cpp ASR library override does not exist: {}",
            path.display()
        ));
    }
    let mut candidates = Vec::new();
    if let Some(parent) = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
    {
        candidates.extend([
            parent.join("nemo_speech_asr_c.dll"),
            parent.join("libnemo_speech_asr_c.so"),
            parent.join("libnemo_speech_asr_c.dylib"),
            parent.join("bin").join("nemo_speech_asr_c.dll"),
        ]);
    }
    if let Some(path) = candidates.into_iter().find(|path| path.is_file()) {
        return Ok(path);
    }
    platform_loader_fallback().ok_or_else(|| {
        anyhow!(
            "NeMo-Speech.cpp runtime was not found beside the application; set VOICEPI_NEMOTRON_LIBRARY or let Whisper Dictate install the verified runtime"
        )
    })
}

// Let the Unix platform loader search LD_LIBRARY_PATH/the loader cache as a
// final fallback. On Windows require an explicit or adjacent path so
// libloading never consults the current working directory.
fn platform_loader_fallback() -> Option<PathBuf> {
    #[cfg(windows)]
    return None;
    #[cfg(target_os = "macos")]
    return Some(PathBuf::from("libnemo_speech_asr_c.dylib"));
    #[cfg(not(any(windows, target_os = "macos")))]
    Some(PathBuf::from("libnemo_speech_asr_c.so"))
}

/// Probe a path through the platform loader without requiring a model or
/// calling any of the exported symbols. On Windows callers pass only explicit
/// or application-adjacent paths; Unix may use a bare soname registered with
/// LD_LIBRARY_PATH or the loader cache.
pub(crate) fn library_is_loadable(path: &Path) -> bool {
    unsafe { load_native_library(path).is_ok() }
}
