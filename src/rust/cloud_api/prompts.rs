//! Shared fixed prompts used by OpenAI-compatible cloud chat calls.
//!
//! Keeping these strings here prevents the production post-processing call
//! and the UI/API health check from drifting apart.

/// System instruction for the post-processing chat completion.
pub(crate) const POSTPROCESS_SYSTEM_PROMPT: &str = "You rewrite dictated text faithfully.";

#[cfg(test)]
mod tests {
    use super::POSTPROCESS_SYSTEM_PROMPT;

    #[test]
    fn postprocess_system_prompt_is_non_empty_and_stable() {
        assert_eq!(
            POSTPROCESS_SYSTEM_PROMPT,
            "You rewrite dictated text faithfully."
        );
    }
}
