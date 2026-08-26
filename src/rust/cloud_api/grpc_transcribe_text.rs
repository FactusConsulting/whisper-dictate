//! Text-boundary helpers for Riva streaming transcripts.
//!
//! Riva may emit finalized segments without boundary whitespace. Latin words
//! need a separator, while CJK scripts and their punctuation conventionally do
//! not. Keeping this policy separate from the transport adapter also keeps the
//! gRPC module below the repository's new-file size guideline.

pub(super) fn append_final_segment(output: &mut String, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let starts_with_attached_punctuation = text.chars().next().is_some_and(is_attached_punctuation);
    let starts_with_cjk = text.chars().next().is_some_and(is_cjk_script);
    let ends_with_cjk = output.chars().next_back().is_some_and(is_cjk_script);
    if !output.is_empty() && !starts_with_attached_punctuation && !starts_with_cjk && !ends_with_cjk
    {
        output.push(' ');
    }
    output.push_str(text);
}

fn is_attached_punctuation(character: char) -> bool {
    matches!(
        character,
        ',' | '.' | '!' | '?' | ';' | ':' | ')' | ']' | '}' | '%' | '\'' | '"'
    ) || matches!(
        character as u32,
        0x3001
            | 0x3002
            | 0xFF0C
            | 0xFF01
            | 0xFF1F
            | 0xFF1B
            | 0xFF1A
            | 0x2026
            | 0x300D
            | 0x300F
            | 0x3011
            | 0xFF09
            | 0xFF3D
            | 0xFF5D
            | 0x2019
            | 0x201D
    )
}

fn is_cjk_script(character: char) -> bool {
    matches!(
        character,
        '\u{1100}'..='\u{11ff}' // Hangul Jamo
            | '\u{2e80}'..='\u{a4cf}' // CJK radicals, Han, Hiragana, Katakana
            | '\u{ac00}'..='\u{d7ff}' // Hangul syllables
            | '\u{f900}'..='\u{faff}' // CJK compatibility ideographs
            | '\u{fe30}'..='\u{fe6f}' // CJK compatibility forms
            | '\u{ff00}'..='\u{ffef}' // full-width CJK forms
            | '\u{20000}'..='\u{3ffff}' // CJK extensions
    )
}

#[cfg(test)]
#[path = "grpc_transcribe_text_tests.rs"]
mod tests;
