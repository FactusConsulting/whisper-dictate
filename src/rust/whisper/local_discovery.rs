//! Auto-discover user-supplied Whisper GGML models in the models cache
//! directory (#332).
//!
//! The curated download catalog in [`super::model_manager`] covers the
//! three CPU-friendly English models the app ships download support for
//! (tiny.en / base.en / small.en). Power users often want to drop a
//! fine-tuned or quantised GGML file into the cache directory by hand
//! and have the runtime pick it up — without editing the catalog or
//! setting `VOICEPI_WHISPER_MODEL_PATH`.
//!
//! This module implements that scan. Given a directory, it walks the
//! immediate children (no recursion — the cache dir is flat) and returns
//! one [`DiscoveredModel`] per file that looks like a whisper.cpp GGML
//! bundle. The consumer decides how to present them:
//!
//! - The `resolve_model_path_from_env` fallback in [`super::dispatch`]
//!   uses the list to pick a custom user model when neither the env-var
//!   override nor a verified catalog entry is present.
//! - The Settings tab's "Whisper model" section renders one row per
//!   discovered file so the user can see the app noticed their file.
//!
//! ## What counts as a model
//!
//! - Extension `.bin` or `.gguf` (case-insensitive), matching what
//!   whisper.cpp accepts.
//! - Size ≥ [`MIN_MODEL_BYTES`] (defaults to 1 MiB) — filters out empty
//!   placeholders and download partials so the picker never surfaces a
//!   zero-byte file.
//! - Filename does NOT end in `.partial` (our own in-flight download
//!   marker; see `partial_path` in `model_manager.rs`).
//! - Filename is NOT one of the curated catalog filenames — those are
//!   already surfaced through the catalog rows, so listing them a second
//!   time would double-count.
//!
//! Non-matches are silently ignored. A directory with a stray README or
//! a screenshot next to the models must produce a clean list, not a
//! parsing error.
//!
//! ## Variant guessing
//!
//! Whisper GGML files traditionally follow the `ggml-<variant>[.en].bin`
//! naming convention (tiny / base / small / medium / large + optional
//! English-only marker). We match on the filename first — it's the
//! authoritative signal when present — and fall back to a coarse
//! size-bucket heuristic for oddly-named files. The variant is a UI hint
//! only; the loader itself doesn't consult it.

use std::fs;
use std::path::{Path, PathBuf};

/// Any file smaller than this in the scan directory is ignored. Real
/// GGML weights start at ~75 MB (tiny.en), so 1 MiB is generous while
/// still filtering out zero-byte placeholders, sentinels and stray
/// `.gitkeep`-style markers. Public so tests and callers can reason
/// about the threshold without magic numbers.
pub const MIN_MODEL_BYTES: u64 = 1024 * 1024;

/// Coarse whisper.cpp model size class. Used purely for UI presentation
/// (`Discovered tiny model — 78 MB`) — the loader treats every
/// discovered file the same way regardless of the variant we guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelVariant {
    Tiny,
    Base,
    Small,
    Medium,
    Large,
    /// Filename didn't contain a well-known keyword AND the file size
    /// didn't fit a familiar bucket. Rendered as "custom" in the UI.
    Unknown,
}

impl ModelVariant {
    /// Short human label for the picker column. Kept as `&'static str`
    /// so no allocation per render.
    pub fn label(self) -> &'static str {
        match self {
            ModelVariant::Tiny => "tiny",
            ModelVariant::Base => "base",
            ModelVariant::Small => "small",
            ModelVariant::Medium => "medium",
            ModelVariant::Large => "large",
            ModelVariant::Unknown => "custom",
        }
    }
}

/// One user-supplied GGML file the discovery pass found under the
/// models directory. `path` is absolute (joined against the scan root
/// which itself is normally absolute) so consumers can open it without
/// re-resolving the cache dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredModel {
    /// Absolute path to the on-disk file.
    pub path: PathBuf,
    /// Just the filename portion (e.g. `ggml-large-v3-q5_0.bin`), used
    /// for display and as the picker's stable identifier.
    pub filename: String,
    /// File size in bytes at scan time.
    pub size_bytes: u64,
    /// Best-effort guess of the size class. Never authoritative — a
    /// fine-tuned file may lie about its variant in the filename.
    pub variant: ModelVariant,
}

/// Enumerate GGML-looking files immediately under `dir`. Returns an
/// empty vector when `dir` doesn't exist, isn't a directory, or contains
/// no matching files — the picker treats "no user models" as an
/// unremarkable state, not an error.
///
/// The list is sorted by filename (case-insensitive) so the picker
/// rendering is deterministic across scans. Callers that want size-
/// sorted output can re-sort the returned vector cheaply.
pub fn discover_models(dir: &Path) -> Vec<DiscoveredModel> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    let catalog_filenames: Vec<&'static str> = super::model_manager::CATALOG
        .iter()
        .map(|e| e.filename)
        .collect();
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if let Some(model) = classify_entry(&path, &catalog_filenames) {
            out.push(model);
        }
    }
    out.sort_by(|a, b| {
        a.filename
            .to_ascii_lowercase()
            .cmp(&b.filename.to_ascii_lowercase())
    });
    out
}

/// Decide whether one directory entry represents a discoverable model
/// and, if so, package it into a [`DiscoveredModel`]. Pure aside from
/// the `metadata()` syscall, so it can be tested via
/// [`discover_models`] against a tempdir.
fn classify_entry(path: &Path, catalog_filenames: &[&str]) -> Option<DiscoveredModel> {
    // Only regular files. `metadata()` (vs `symlink_metadata()`) follows
    // symlinks so a user who symlinks their models directory into the
    // cache dir still sees them.
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let filename = path.file_name()?.to_str()?.to_owned();
    if !has_ggml_extension(&filename) {
        return None;
    }
    if filename.to_ascii_lowercase().ends_with(".partial") {
        return None;
    }
    if catalog_filenames.iter().any(|c| c == &filename) {
        return None;
    }
    let size_bytes = meta.len();
    if size_bytes < MIN_MODEL_BYTES {
        return None;
    }
    let variant = guess_variant(&filename, size_bytes);
    Some(DiscoveredModel {
        path: path.to_path_buf(),
        filename,
        size_bytes,
        variant,
    })
}

/// True iff `filename` ends in `.bin` or `.gguf` (case-insensitive).
/// Broken out so the same rule applies to the discovery pass and any
/// future drag-and-drop file-picker validation.
pub fn has_ggml_extension(filename: &str) -> bool {
    let lower = filename.to_ascii_lowercase();
    lower.ends_with(".bin") || lower.ends_with(".gguf")
}

/// Guess the size class from filename hints, falling back to raw byte
/// count buckets when the name is uninformative. The size buckets are
/// deliberately generous (±30% of the canonical size) so a quantised
/// variant still lands in the right bucket.
pub fn guess_variant(filename: &str, size_bytes: u64) -> ModelVariant {
    let lower = filename.to_ascii_lowercase();
    // Order matters: check "large" before "base" etc. so a filename
    // like `ggml-large-en.bin` doesn't match on a substring further up
    // the list. All the keywords here happen to be non-overlapping, but
    // scanning in size order still reads as intentional.
    for (needle, variant) in [
        ("tiny", ModelVariant::Tiny),
        ("base", ModelVariant::Base),
        ("small", ModelVariant::Small),
        ("medium", ModelVariant::Medium),
        ("large", ModelVariant::Large),
    ] {
        if lower.contains(needle) {
            return variant;
        }
    }
    // Filename didn't tell us anything useful. Canonical GGML sizes at
    // the F16 baseline (from ggerganov/whisper.cpp):
    //   tiny    ~39 MB / tiny.en   ~78 MB
    //   base    ~74 MB / base.en  ~148 MB
    //   small  ~244 MB / small.en ~488 MB
    //   medium ~769 MB / medium.en ~1.5 GB
    //   large   ~1.5 GB
    // Widen the boundaries so 4-/5-/8-bit quantisations still map to
    // their source variant — a Q5_0 large is ~1 GB, Q4_0 small is ~180
    // MB, etc.
    const MB: u64 = 1_000_000;
    match size_bytes {
        0..=99_000_000 => ModelVariant::Tiny,     // ≤ ~99 MB → tiny-ish
        n if n <= 200 * MB => ModelVariant::Base, // ≤ ~200 MB
        n if n <= 600 * MB => ModelVariant::Small, // ≤ ~600 MB
        n if n <= 1_200 * MB => ModelVariant::Medium, // ≤ ~1.2 GB
        _ => ModelVariant::Large,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Plant a synthetic file of the requested size at `path`. Writes a
    /// single byte and then seeks so the OS reports the requested length
    /// without us allocating hundreds of megabytes in RAM for a test.
    fn make_file(path: &Path, size_bytes: u64) {
        let mut f = fs::File::create(path).expect("create test file");
        if size_bytes > 0 {
            f.set_len(size_bytes).expect("set_len");
            // Touch one byte at position 0 so the file isn't a sparse
            // zero-byte file on filesystems that treat set_len(0..) as
            // a no-op.
            f.write_all(&[0u8]).expect("write");
        }
    }

    #[test]
    fn discovers_bin_and_gguf_files() {
        let tmp = tempfile::tempdir().unwrap();
        make_file(&tmp.path().join("my-model.bin"), 5 * 1024 * 1024);
        make_file(&tmp.path().join("other.gguf"), 5 * 1024 * 1024);
        let models = discover_models(tmp.path());
        assert_eq!(models.len(), 2, "both files should be discovered");
        assert!(models.iter().any(|m| m.filename == "my-model.bin"));
        assert!(models.iter().any(|m| m.filename == "other.gguf"));
    }

    #[test]
    fn ignores_non_model_files() {
        let tmp = tempfile::tempdir().unwrap();
        make_file(&tmp.path().join("readme.txt"), 5 * 1024 * 1024);
        make_file(&tmp.path().join("cover.png"), 5 * 1024 * 1024);
        make_file(&tmp.path().join(".gitkeep"), 5 * 1024 * 1024);
        make_file(&tmp.path().join("real-model.bin"), 5 * 1024 * 1024);
        let models = discover_models(tmp.path());
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].filename, "real-model.bin");
    }

    #[test]
    fn ignores_tiny_placeholder_files() {
        let tmp = tempfile::tempdir().unwrap();
        make_file(&tmp.path().join("empty.bin"), 0);
        make_file(&tmp.path().join("stub.bin"), 100); // under 1 MiB
        assert!(discover_models(tmp.path()).is_empty());
    }

    #[test]
    fn ignores_partial_downloads() {
        let tmp = tempfile::tempdir().unwrap();
        // Emulate `partial_path`'s `<name>.<pid>-<seq>.partial` shape.
        make_file(
            &tmp.path().join("ggml-huge.bin.12345-0.partial"),
            10 * 1024 * 1024,
        );
        make_file(&tmp.path().join("ggml-huge.bin"), 10 * 1024 * 1024);
        let models = discover_models(tmp.path());
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].filename, "ggml-huge.bin");
    }

    #[test]
    fn ignores_catalog_filenames_to_avoid_double_counting() {
        // The curated catalog already renders these entries; adding them
        // to the discovered list would show the tiny.en row twice.
        let tmp = tempfile::tempdir().unwrap();
        make_file(&tmp.path().join("ggml-tiny.en.bin"), 5 * 1024 * 1024);
        make_file(&tmp.path().join("ggml-base.en.bin"), 5 * 1024 * 1024);
        make_file(&tmp.path().join("custom-model.bin"), 5 * 1024 * 1024);
        let models = discover_models(tmp.path());
        assert_eq!(
            models.len(),
            1,
            "catalog files must be filtered so only custom-model.bin remains"
        );
        assert_eq!(models[0].filename, "custom-model.bin");
    }

    #[test]
    fn returns_empty_when_directory_missing() {
        let missing = Path::new("/definitely-not-a-real-dir/xyz-nope");
        assert!(discover_models(missing).is_empty());
    }

    #[test]
    fn returns_empty_when_target_is_not_a_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("i-am-a-file.bin");
        make_file(&file, 5 * 1024 * 1024);
        // Ask to "discover" inside the file itself — must not panic and
        // must return empty.
        assert!(discover_models(&file).is_empty());
    }

    #[test]
    fn results_are_sorted_by_filename_case_insensitive() {
        let tmp = tempfile::tempdir().unwrap();
        make_file(&tmp.path().join("Zeta.bin"), 5 * 1024 * 1024);
        make_file(&tmp.path().join("alpha.bin"), 5 * 1024 * 1024);
        make_file(&tmp.path().join("mid.bin"), 5 * 1024 * 1024);
        let models = discover_models(tmp.path());
        let names: Vec<&str> = models.iter().map(|m| m.filename.as_str()).collect();
        assert_eq!(names, vec!["alpha.bin", "mid.bin", "Zeta.bin"]);
    }

    #[test]
    fn variant_guess_matches_filename_keywords() {
        assert_eq!(
            guess_variant("ggml-tiny.en.bin", 78_000_000),
            ModelVariant::Tiny
        );
        assert_eq!(
            guess_variant("ggml-BASE.bin", 148_000_000),
            ModelVariant::Base
        );
        assert_eq!(
            guess_variant("ggml-small.en.bin", 488_000_000),
            ModelVariant::Small
        );
        assert_eq!(
            guess_variant("ggml-medium.q5_0.bin", 500_000_000),
            ModelVariant::Medium
        );
        assert_eq!(
            guess_variant("my-large-finetune-v3.gguf", 1_800_000_000),
            ModelVariant::Large
        );
    }

    #[test]
    fn variant_guess_falls_back_to_size_buckets() {
        // No keyword in the filename → size bucket rules.
        assert_eq!(guess_variant("custom.bin", 50_000_000), ModelVariant::Tiny);
        assert_eq!(guess_variant("custom.bin", 150_000_000), ModelVariant::Base);
        assert_eq!(
            guess_variant("custom.bin", 400_000_000),
            ModelVariant::Small
        );
        assert_eq!(
            guess_variant("custom.bin", 1_000_000_000),
            ModelVariant::Medium
        );
        assert_eq!(
            guess_variant("custom.bin", 2_500_000_000),
            ModelVariant::Large
        );
    }

    #[test]
    fn variant_label_covers_every_variant() {
        // Trip-wire: adding a variant without a label would slip through
        // the picker silently. Force the exhaustive match here.
        for v in [
            ModelVariant::Tiny,
            ModelVariant::Base,
            ModelVariant::Small,
            ModelVariant::Medium,
            ModelVariant::Large,
            ModelVariant::Unknown,
        ] {
            let label = v.label();
            assert!(
                !label.is_empty(),
                "variant {v:?} must have a non-empty label"
            );
        }
    }

    #[test]
    fn discovered_model_carries_absolute_path_and_variant() {
        let tmp = tempfile::tempdir().unwrap();
        let name = "ggml-large-v3-q5_0.bin";
        make_file(&tmp.path().join(name), 1_100_000_000);
        let mut models = discover_models(tmp.path());
        assert_eq!(models.len(), 1);
        let m = models.pop().unwrap();
        assert_eq!(m.filename, name);
        assert!(m.path.ends_with(name));
        assert_eq!(m.variant, ModelVariant::Large);
        assert_eq!(m.size_bytes, 1_100_000_000);
    }

    #[test]
    fn has_ggml_extension_is_case_insensitive() {
        assert!(has_ggml_extension("x.bin"));
        assert!(has_ggml_extension("x.BIN"));
        assert!(has_ggml_extension("x.Gguf"));
        assert!(!has_ggml_extension("x.txt"));
        assert!(!has_ggml_extension("x.bin.txt"));
        assert!(!has_ggml_extension("nofile"));
    }
}
