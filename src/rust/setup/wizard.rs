//! Stream-driven interactive setup wizard.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};

use anyhow::{anyhow, Result};

fn current_value(
    setting: &crate::config::RuntimeSetting,
    existing: &BTreeMap<String, String>,
) -> String {
    existing
        .get(&setting.key)
        .cloned()
        .or_else(|| setting.default.clone())
        .unwrap_or_default()
}

fn validate_answer(setting: &crate::config::RuntimeSetting, answer: &str) -> Result<String> {
    let answer = if answer == "auto" && setting.choices.iter().any(String::is_empty) {
        ""
    } else {
        answer
    };
    if !setting.choices.is_empty() && !setting.choices.iter().any(|choice| choice == answer) {
        let choices = setting
            .choices
            .iter()
            .map(|choice| if choice.is_empty() { "auto" } else { choice })
            .collect::<Vec<_>>()
            .join(", ");
        return Err(anyhow!("must be one of: {choices}"));
    }
    if setting.min.is_some() || setting.max.is_some() {
        let value = answer
            .parse::<f64>()
            .map_err(|_| anyhow!("must be a number"))?;
        if !value.is_finite() {
            return Err(anyhow!("must be a finite number"));
        }
        if setting.min.is_some_and(|min| value < min) {
            return Err(anyhow!("must be >= {}", setting.min.unwrap()));
        }
        if setting.max.is_some_and(|max| value > max) {
            return Err(anyhow!("must be <= {}", setting.max.unwrap()));
        }
    }
    Ok(answer.to_owned())
}

fn read_answer(input: &mut impl BufRead, output: &mut impl Write, prompt: &str) -> Result<String> {
    write!(output, "{prompt}")?;
    output.flush()?;
    let mut answer = String::new();
    input.read_line(&mut answer)?;
    Ok(answer.trim().to_owned())
}

fn prompt_setting(
    setting: &crate::config::RuntimeSetting,
    existing: &BTreeMap<String, String>,
    selected: &mut BTreeMap<String, String>,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<()> {
    let current = current_value(setting, existing);
    writeln!(output, "\n{}", setting.description)?;
    if !setting.choices.is_empty() {
        let choices = setting
            .choices
            .iter()
            .map(|choice| if choice.is_empty() { "(auto)" } else { choice })
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(output, "  choices: {choices}")?;
    }
    loop {
        let shown = if current.is_empty() {
            "(unset)"
        } else {
            &current
        };
        let answer = read_answer(input, output, &format!("  {} [{shown}]: ", setting.key))?;
        if answer.is_empty() {
            return Ok(());
        }
        match validate_answer(setting, &answer) {
            Ok(value) => {
                if (setting.nullable && value.is_empty())
                    || (Some(&value) != setting.default.as_ref() && !value.is_empty())
                {
                    selected.insert(setting.key.clone(), value);
                } else {
                    selected.remove(&setting.key);
                }
                return Ok(());
            }
            Err(error) => writeln!(output, "  ! {error}")?,
        }
    }
}

pub fn run(
    existing: &BTreeMap<String, String>,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<BTreeMap<String, String>> {
    // Preserve every existing non-default effective value, including advanced
    // settings when the user chooses not to walk the advanced prompts.
    let mut selected = existing
        .iter()
        .filter(|(key, value)| {
            crate::config::runtime_settings()
                .iter()
                .find(|setting| setting.key == **key)
                .is_some_and(|setting| {
                    (setting.nullable && value.is_empty())
                        || (!value.is_empty() && Some(*value) != setting.default.as_ref())
                })
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<BTreeMap<_, _>>();
    writeln!(output, "wd setup - basic settings")?;
    writeln!(
        output,
        "Press ENTER to keep the shown value; type to change it."
    )?;
    for setting in crate::config::runtime_settings()
        .iter()
        .filter(|setting| !setting.advanced)
    {
        prompt_setting(setting, existing, &mut selected, input, output)?;
    }
    let advanced = read_answer(input, output, "\nRun advanced setup? [y/N]: ")?;
    if matches!(advanced.to_ascii_lowercase().as_str(), "y" | "yes") {
        let mut category = "";
        for setting in crate::config::runtime_settings()
            .iter()
            .filter(|setting| setting.advanced)
        {
            if setting.category != category {
                category = &setting.category;
                writeln!(output, "\n--- {category} ---")?;
            }
            prompt_setting(setting, existing, &mut selected, input, output)?;
        }
    }
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setting(key: &str) -> &'static crate::config::RuntimeSetting {
        crate::config::runtime_settings()
            .iter()
            .find(|setting| setting.key == key)
            .unwrap()
    }

    #[test]
    fn choices_and_numeric_bounds_are_enforced() {
        assert!(validate_answer(setting("stt_backend"), "invalid")
            .unwrap_err()
            .to_string()
            .contains("one of"));
        assert_eq!(
            validate_answer(setting("stt_backend"), "openai").unwrap(),
            "openai"
        );
        assert!(validate_answer(setting("max_chars_per_second"), "501")
            .unwrap_err()
            .to_string()
            .contains("<="));
        assert!(validate_answer(setting("max_chars_per_second"), "nan")
            .unwrap_err()
            .to_string()
            .contains("finite"));
    }

    #[test]
    fn scripted_basic_setup_keeps_non_defaults_and_changes_values() {
        let settings = crate::config::runtime_settings();
        let basic_count = settings.iter().filter(|setting| !setting.advanced).count();
        let mut answers = vec![String::new(); basic_count];
        let model_index = settings
            .iter()
            .filter(|setting| !setting.advanced)
            .position(|setting| setting.key == "model")
            .unwrap();
        answers[model_index] = "large-v3".to_owned();
        answers.push("n".to_owned());
        let script = format!("{}\n", answers.join("\n"));
        let mut input = std::io::Cursor::new(script);
        let mut output = Vec::new();
        let result = run(&BTreeMap::new(), &mut input, &mut output).unwrap();
        assert_eq!(result.get("model").map(String::as_str), Some("large-v3"));
    }

    #[test]
    fn skipping_advanced_setup_preserves_existing_non_defaults() {
        let settings = crate::config::runtime_settings();
        let mut answers =
            vec![String::new(); settings.iter().filter(|setting| !setting.advanced).count()];
        answers.push("n".to_owned());
        let mut input = std::io::Cursor::new(format!("{}\n", answers.join("\n")));
        let mut output = Vec::new();
        let existing = BTreeMap::from([("max_chars_per_second".to_owned(), "25".to_owned())]);
        let result = run(&existing, &mut input, &mut output).unwrap();
        assert_eq!(
            result.get("max_chars_per_second").map(String::as_str),
            Some("25")
        );
    }

    #[test]
    fn setup_preserves_nullable_clear_markers_when_enter_keeps_values() {
        let settings = crate::config::runtime_settings();
        let mut answers =
            vec![String::new(); settings.iter().filter(|setting| !setting.advanced).count()];
        answers.push("n".to_owned());
        let mut input = std::io::Cursor::new(format!("{}\n", answers.join("\n")));
        let mut output = Vec::new();
        let existing = BTreeMap::from([("lang".to_owned(), String::new())]);

        let result = run(&existing, &mut input, &mut output).unwrap();

        assert_eq!(result.get("lang").map(String::as_str), Some(""));
    }
}
