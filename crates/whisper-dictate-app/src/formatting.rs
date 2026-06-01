use regex::RegexBuilder;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatCommandResult {
    pub text: String,
    pub enabled: bool,
    pub changed: bool,
    pub command_set: String,
    pub applied: Vec<AppliedFormatCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedFormatCommand {
    pub command: &'static str,
    pub replacement: &'static str,
    pub count: usize,
}

const EN_COMMANDS: &[(&str, &str)] = &[
    ("new paragraph", "\n\n"),
    ("new line", "\n"),
    ("newline", "\n"),
    ("bullet list", "\n- "),
    ("bullet point", "\n- "),
    ("comma", ","),
    ("period", "."),
    ("full stop", "."),
    ("question mark", "?"),
    ("exclamation mark", "!"),
    ("colon", ":"),
    ("semicolon", ";"),
    ("dash", "-"),
    ("hyphen", "-"),
];

const DA_COMMANDS: &[(&str, &str)] = &[
    ("nyt afsnit", "\n\n"),
    ("ny linje", "\n"),
    ("linjeskift", "\n"),
    ("punktliste", "\n- "),
    ("punktopstilling", "\n- "),
    ("komma", ","),
    ("punktum", "."),
    ("spørgsmålstegn", "?"),
    ("sporgsmålstegn", "?"),
    ("udråbstegn", "!"),
    ("udraabstegn", "!"),
    ("kolon", ":"),
    ("semikolon", ";"),
    ("bindestreg", "-"),
];

pub fn normalize_command_set(raw: Option<&str>) -> String {
    let selected = raw.unwrap_or("off").trim().to_lowercase();
    match selected.as_str() {
        "" | "0" | "false" | "no" | "off" => "off".to_owned(),
        "all" => "both".to_owned(),
        "en" | "da" | "both" => selected,
        _ if truthy(&selected) => "both".to_owned(),
        _ => "off".to_owned(),
    }
}

pub fn apply_format_commands(text: &str, command_set: Option<&str>) -> FormatCommandResult {
    let selected = normalize_command_set(command_set);
    if selected == "off" {
        return FormatCommandResult {
            text: text.to_owned(),
            enabled: false,
            changed: false,
            command_set: selected,
            applied: Vec::new(),
        };
    }

    let mut out = text.to_owned();
    let mut applied = Vec::new();
    let mut commands = commands_for(&selected);
    commands.sort_by(|a, b| b.0.chars().count().cmp(&a.0.chars().count()));

    for (command, replacement) in commands {
        let (next, count) = apply_phrase(&out, command, replacement);
        if count > 0 {
            out = next;
            applied.push(AppliedFormatCommand {
                command,
                replacement,
                count,
            });
        }
    }

    if !applied.is_empty() {
        out = tidy(&out);
    }

    FormatCommandResult {
        changed: out != text,
        text: out,
        enabled: true,
        command_set: selected,
        applied,
    }
}

fn commands_for(command_set: &str) -> Vec<(&'static str, &'static str)> {
    let mut commands = Vec::new();
    if command_set == "en" || command_set == "both" {
        commands.extend(EN_COMMANDS);
    }
    if command_set == "da" || command_set == "both" {
        commands.extend(DA_COMMANDS);
    }
    commands
}

fn apply_phrase(text: &str, phrase: &str, replacement: &str) -> (String, usize) {
    let pattern = phrase
        .split_whitespace()
        .map(regex::escape)
        .collect::<Vec<_>>()
        .join(r"\s+");
    let regex = RegexBuilder::new(&pattern)
        .case_insensitive(true)
        .build()
        .expect("format command regex must compile");

    let mut out = String::with_capacity(text.len());
    let mut last_end = 0;
    let mut count = 0;

    for matched in regex.find_iter(text) {
        if matched.start() < last_end
            || has_word_char_before(text, matched.start())
            || has_word_char_after(text, matched.end())
        {
            continue;
        }

        let end = consume_trailing_command_junk(text, matched.end());
        out.push_str(&text[last_end..matched.start()]);
        out.push_str(replacement);
        last_end = end;
        count += 1;
    }

    if count == 0 {
        return (text.to_owned(), 0);
    }

    out.push_str(&text[last_end..]);
    (out, count)
}

fn has_word_char_before(text: &str, index: usize) -> bool {
    text[..index].chars().next_back().is_some_and(is_word_char)
}

fn has_word_char_after(text: &str, index: usize) -> bool {
    text[index..].chars().next().is_some_and(is_word_char)
}

fn is_word_char(ch: char) -> bool {
    ch == '_' || ch.is_alphanumeric()
}

fn consume_trailing_command_junk(text: &str, index: usize) -> usize {
    let mut end = index;
    for (offset, ch) in text[index..].char_indices() {
        if matches!(ch, ' ' | '\t' | ',' | '.' | '!' | '?' | ';' | ':') {
            end = index + offset + ch.len_utf8();
        } else {
            break;
        }
    }
    end
}

fn tidy(text: &str) -> String {
    let text = replace_all(text, r"[ \t]+\n", "\n");
    let text = replace_all(&text, r"\n[ \t]+", "\n");
    let text = replace_all(&text, r" *([,.;:!?])", "$1");
    let text = replace_all(&text, r"([,.;:!?])(\S)", "$1 $2");
    let text = replace_all(&text, r" *- *", " - ");
    let text = replace_all(&text, r"\n - ", "\n- ");
    let text = replace_all(&text, r"\n{3,}", "\n\n");
    text.trim().to_owned()
}

fn replace_all(text: &str, pattern: &str, replacement: &str) -> String {
    regex::Regex::new(pattern)
        .expect("format tidy regex must compile")
        .replace_all(text, replacement)
        .into_owned()
}

fn truthy(value: &str) -> bool {
    !matches!(value, "" | "0" | "false" | "no" | "off")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_commands_are_off_by_default() {
        let result = apply_format_commands("write comma literally", None);

        assert!(!result.enabled);
        assert_eq!(result.text, "write comma literally");
        assert_eq!(result.command_set, "off");
    }

    #[test]
    fn english_format_commands_replace_whole_phrases() {
        let result =
            apply_format_commands("first item comma new line second item period", Some("en"));

        assert!(result.enabled);
        assert!(result.changed);
        assert_eq!(result.text, "first item,\nsecond item.");
        assert!(result.applied.contains(&AppliedFormatCommand {
            command: "new line",
            replacement: "\n",
            count: 1,
        }));
    }

    #[test]
    fn danish_format_commands_replace_whole_phrases() {
        let result = apply_format_commands(
            "første punkt komma ny linje andet punkt punktum",
            Some("da"),
        );

        assert_eq!(result.text, "første punkt,\nandet punkt.");
    }

    #[test]
    fn format_commands_do_not_replace_inside_words() {
        let result =
            apply_format_commands("Common words and kommandolinje stay literal", Some("both"));

        assert!(!result.changed);
        assert_eq!(result.text, "Common words and kommandolinje stay literal");
    }

    #[test]
    fn all_alias_and_truthy_unknown_values_enable_both_languages() {
        assert_eq!(normalize_command_set(Some("all")), "both");
        assert_eq!(normalize_command_set(Some("true")), "both");
    }

    #[test]
    fn longer_commands_run_before_shorter_commands() {
        let result = apply_format_commands("first new paragraph second new line third", Some("en"));

        assert_eq!(result.text, "first\n\nsecond\nthird");
    }
}
