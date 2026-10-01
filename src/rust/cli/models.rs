use clap::Subcommand;

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum ModelsCommand {
    /// List the curated download catalog with each entry's name, file size,
    /// description, and whether a verified copy is already in the user cache.
    List,
    /// Download a catalog entry into the user cache. Verifies SHA-256 after
    /// the download completes; on mismatch the partial file is deleted.
    Download {
        /// Catalog name (e.g. `tiny.en`, `base.en`, `small.en`). Use
        /// `models list` to see what's available.
        name: String,
    },
    /// Print the cache directory where downloaded models are stored.
    Path,
}
