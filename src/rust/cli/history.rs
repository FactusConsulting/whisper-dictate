use clap::Subcommand;

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum HistoryCommand {
    /// List recent history rows (human-readable summary tail).
    List {
        /// Number of rows to show.
        #[arg(default_value_t = 10)]
        limit: usize,
    },
    /// Print the most recent N transcripts (newest first). By default emits
    /// only the `text` field, one entry per line - pass `--json` to get the
    /// full entry objects as a JSON array.
    Last {
        /// Number of transcripts to print (default 1). Values <=0 are
        /// clamped to 1 so scripts can pass through user input safely.
        #[arg(long, default_value_t = 1)]
        n: usize,
        /// Emit a JSON array of the entries instead of plain text.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Copy the most recent transcript to the system clipboard. Uses the
    /// first available OS backend (`wl-copy` / `xclip` on Linux, `clip.exe`
    /// on Windows, `pbcopy` on macOS). Prints `copied: <text>` on success.
    /// Exits non-zero when the history is empty or no clipboard tool is
    /// available.
    #[command(name = "copy-last")]
    CopyLast,
    /// Feed the last transcript back into the injection pipeline. Wraps
    /// `inject-text` - same backend selection and dry-run guardrails - so
    /// this verb **defaults to a dry-run**: it prints the plan and does
    /// nothing else. Pass `--do-it` (alias `--live`) to actually type.
    #[command(name = "reinject-last")]
    ReinjectLast {
        /// Explicit dry-run flag (matches the default). Set for clarity
        /// self-documenting shell scripts; `--do-it` overrides.
        #[arg(long, default_value_t = false)]
        dry_run: bool,
        /// REALLY inject the text into the active window (dangerous
        /// moves the cursor, types keys). Off by default.
        #[arg(long, alias = "live", default_value_t = false)]
        do_it: bool,
        /// Machine-readable JSON output (single line) of the injection plan.
        #[arg(long, default_value_t = false)]
        json: bool,
        /// Backend selector; forwarded to `inject-text`. Default `auto`.
        #[arg(
            long,
            default_value = "auto",
            value_parser = [
                "auto", "pynput", "wtype", "ydotool", "xdotool", "kwtype",
                "dotool", "enigo", "type", "paste",
            ],
        )]
        backend: String,
    },
    /// Substring search over transcripts (case-insensitive). Newest matches
    /// first, up to `--limit` (default 20). JSON output is a JSON array of
    /// full entries.
    Search {
        /// Substring to search for in the `text` field.
        query: String,
        /// Cap on the number of matches returned. Values <=0 clamp to 1.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Emit a JSON array of the matching entries instead of plain text.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}
