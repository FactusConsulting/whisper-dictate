use clap::Subcommand;

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum DevicesCommand {
    /// Dry-run open the named microphone and print a single JSON usability
    /// result. Resolves `name` against the live device list (case-insensitive
    /// substring - same rule the picker uses), then tries the same
    /// WASAPI/DirectSound/MME open matrix as live capture (opening and
    /// immediately closing each candidate, capturing no audio). Loads no ML
    /// model. Pass an empty string to test the system default input.
    ///
    /// Output shape (single JSON object, one line):
    /// `{"device":"...", "usable":true|false, "endpoint":"wasapi|directsound|mme|default|null", "samplerate":<int|null>, "dtype":"...|null", "resampled":true|false, "reason":"...|null"}`.
    /// A non-usable device still exits 0 - it's a normal reportable outcome,
    /// not a CLI error. Only sub-process launch failures return non-zero.
    Test {
        /// Microphone name (case-insensitive substring; same matching rule as
        /// the picker). Pass `""` to test the system default input.
        ///
        /// `allow_hyphen_values` lets rare device names that start with `-`
        /// through clap so the device resolver can decide, rather than clap
        /// rejecting the value as a stray flag.
        #[arg(allow_hyphen_values = true)]
        name: String,
    },
}
