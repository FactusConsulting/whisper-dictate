use super::post::GROQ_POST_MODEL_HELP;

#[test]
fn groq_post_model_help_describes_only_current_picker_choices() {
    assert!(GROQ_POST_MODEL_HELP.contains("recommended fast cleanup"));
    assert!(GROQ_POST_MODEL_HELP.contains("heavier highest-quality"));
    for retired_category in ["Danish", "reasoning", "preview"] {
        assert!(
            !GROQ_POST_MODEL_HELP.contains(retired_category),
            "help still advertises removed category {retired_category:?}"
        );
    }
}
