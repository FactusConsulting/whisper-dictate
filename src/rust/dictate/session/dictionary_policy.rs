//! dictionary policy for the session-owned utterance lifecycle.

use super::*;

impl<T: TranscribeBackend, I: InjectBackend> DictateSession<T, I> {
    /// Apply the attached dictionary's replacement table to `text`, returning
    /// the rewritten string and the per-replacement change records (for the
    /// utterance event's `dictionary_replacements` field). The table is
    /// resolved through the provider ([`crate::dictionary::DictionaryProvider::current`]),
    /// so a reloading provider re-reads it here at the utterance boundary. A
    /// `None` provider, an empty replacement table, or empty text is a
    /// passthrough (no changes); a replacement regex error keeps the original
    /// text (a replacement failure must never drop a dictation). Takes `&mut
    /// self` because the provider may mutate its reload cache.
    pub(super) fn apply_dictionary(
        &mut self,
        text: &str,
    ) -> (
        String,
        Vec<crate::dictionary::ReplacementChange>,
        Option<String>,
    ) {
        match &mut self.dictionary {
            Some(provider) => {
                let dictionary = provider.current();
                let replacements = if text.is_empty() {
                    (text.to_owned(), Vec::new())
                } else {
                    dictionary
                        .apply_replacements(text)
                        .unwrap_or_else(|_| (text.to_owned(), Vec::new()))
                };
                let load_error = provider.take_load_error();
                (replacements.0, replacements.1, load_error)
            }
            None => (text.to_owned(), Vec::new(), None),
        }
    }
}
