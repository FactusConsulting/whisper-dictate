use clap::Subcommand;

#[derive(Debug, Clone, PartialEq, Subcommand)]
pub enum DictionaryCommand {
    /// Print dictionary path, term count, replacement count and prompt preview.
    Status,
    /// Create the dictionary if needed and open it in the platform editor.
    Open,
    /// Add a prompt vocabulary term.
    Add {
        /// Term to add.
        term: String,
    },
    /// Add or update a deterministic replacement in FROM=TO form.
    Replace {
        /// Replacement mapping, for example "lead death=lead dev".
        mapping: String,
    },
    /// Print the Whisper `initial_prompt` string that would be sent for
    /// dictations against the current dictionary + config. Useful for
    /// eyeballing whether a term / replacement list produces a sane prompt
    /// without starting the full runtime.
    ///
    /// Reads the dictionary at `--dictionary` (or `$VOICEPI_DICTIONARY`, or
    /// the per-user default) and the base prompt from `config.json`
    /// (`initial_prompt`). `--max-length` overrides the character cap so
    /// you can inspect how a smaller / larger budget would trim the
    /// vocabulary list. `--json` emits a machine-readable payload with
    /// `prompt`, `length_chars`, `term_count`, `truncated`, `source`.
    #[command(name = "prompt")]
    Prompt {
        /// Dictionary file to read. Default: `$VOICEPI_DICTIONARY` or the
        /// per-user `dictionary.json`. A missing file is treated as an
        /// empty dictionary (fresh installs before the first term is
        /// added).
        #[arg(long, value_name = "PATH")]
        dictionary: Option<String>,
        /// Emit machine-readable JSON to stdout.
        #[arg(long)]
        json: bool,
        /// Override the prompt character cap (default: config
        /// `dictionary_prompt_chars`, typically 1200). Passing a small
        /// value here is a fast way to preview how term-list truncation
        /// would land.
        #[arg(long, value_name = "N")]
        max_length: Option<usize>,
    },
    /// Print the raw terms + replacements the runtime loaded from the
    /// dictionary. Bonus adapter around the same loader `prompt` uses; it
    /// performs no network access.
    #[command(name = "list")]
    List {
        /// Dictionary file to read. Default: `$VOICEPI_DICTIONARY` or the
        /// per-user `dictionary.json`.
        #[arg(long, value_name = "PATH")]
        dictionary: Option<String>,
        /// Emit machine-readable JSON to stdout.
        #[arg(long)]
        json: bool,
    },
    /// Extract domain terms from the golden-corpus reference TEXT (curated
    /// terms + capitalised/multi-word/technical tokens) and append+dedup them
    /// into the dictionary. PREVIEW by default; pass `--apply` to write.
    /// Reads corpus TEXT only - it never records or touches audio. Honours
    /// `--language` / `--category` for profile selection.
    #[command(name = "build-from-corpus")]
    BuildFromCorpus {
        /// Path to the corpus manifest (`benchmark/corpus.json`). When omitted
        /// the resolver searches `<app_root>/benchmark/corpus.json` and the
        /// per-user appdata equivalent.
        #[arg(long = "benchmark-corpus", value_name = "PATH")]
        benchmark_corpus: Option<String>,
        /// Override the app root used to resolve the corpus manifest. Hidden
        /// because it exists for repository tests.
        #[arg(long, hide = true)]
        app_root: Option<String>,
        /// Dictionary file to read / append. Default: `$VOICEPI_DICTIONARY` or
        /// the per-user `dictionary.json`.
        #[arg(long, value_name = "PATH")]
        dictionary: Option<String>,
        /// Profile selector: restrict to these languages (e.g. `da` or
        /// `da,en`).
        #[arg(long)]
        language: Option<String>,
        /// Profile selector: restrict to these categories or friendly groups
        /// (e.g. `technical`, `names`, `mixed_technical`).
        #[arg(long)]
        category: Option<String>,
        /// Minimum corpus occurrence count for a term to be proposed (default
        /// 1).
        #[arg(long, default_value_t = 1)]
        min_count: usize,
        /// WRITE the changes instead of only previewing them.
        #[arg(long)]
        apply: bool,
        /// Emit machine-readable JSON to stdout.
        #[arg(long)]
        json: bool,
    },
    /// Read an annotated benchmark JSONL and SUGGEST the domain terms the
    /// model missed (`term_misses`) as dictionary additions. PREVIEW by
    /// default; pass `--apply` to add the new terms. Reads result TEXT only
    /// never records audio.
    #[command(name = "suggest-terms")]
    SuggestTerms {
        /// Path to the annotated benchmark JSONL emitted by
        /// `wd bench`.
        jsonl: String,
        /// Dictionary file to read / append. Default: `$VOICEPI_DICTIONARY` or
        /// the per-user `dictionary.json`.
        #[arg(long, value_name = "PATH")]
        dictionary: Option<String>,
        /// Minimum miss count for a term to be suggested (default 1).
        #[arg(long, default_value_t = 1)]
        min_count: usize,
        /// WRITE the new terms instead of only previewing them.
        #[arg(long)]
        apply: bool,
        /// Emit machine-readable JSON to stdout.
        #[arg(long)]
        json: bool,
    },
    /// Read a benchmark / history JSONL and SUGGEST fuzzy REPLACEMENT pairs
    /// (`from -> to`) mined from the transcripts - the fuzzy sibling of
    /// `suggest-terms` (which proposes term additions). PREVIEW only: the
    /// output is never written; a human/automation reviews it and then adds
    /// the accepted pairs via `dictionary replace FROM=TO`.
    ///
    /// Reads the live dictionary snapshot (via `--dictionary` / the same
    /// resolver as `dictionary status`) so already-known replacements and
    /// existing dictionary terms are filtered out. The scoring rules mirror
    /// the retired compatibility surface byte-for-byte (see
    /// `docs/ARCHITECTURE.md`).
    #[command(name = "suggest-replacements")]
    SuggestReplacements {
        /// Path to the JSONL to scan (benchmark result rows or history rows).
        jsonl: String,
        /// Dictionary file to read for the "already known" snapshot. Default:
        /// `$VOICEPI_DICTIONARY` or the per-user `dictionary.json`.
        #[arg(long, value_name = "PATH")]
        dictionary: Option<String>,
        /// Minimum fuzzy-match confidence for a suggestion to surface (0.0 ...
        /// 1.0; default 0.62).
        #[arg(long, default_value_t = 0.62)]
        min_confidence: f64,
        /// Emit a JSON array of suggestions instead of the human preview.
        #[arg(long)]
        json: bool,
    },
}
