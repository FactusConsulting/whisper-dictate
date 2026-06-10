use super::super::*;
use super::*;

impl WhisperDictateApp {
    pub(in crate::ui) fn profiles_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Profiles");
        let palette = ui_palette(&self.settings.ui_theme);
        ui.label(
            egui::RichText::new(
                "Profiles override settings per target app. When the focused window's \
                 title or process matches a profile, that profile's settings apply just \
                 for that window — e.g. English + paste mode in your code editor, or a \
                 different language in Slack — and revert when you switch away.",
            )
            .color(palette.text_muted),
        );
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "Format: a JSON array of objects, each with \"name\", a \"match\" on \
                 \"title\"/\"process\" (a string or list; case-insensitive substring), and \
                 \"settings\" (config keys to override). The first matching profile wins; \
                 the active one is logged as \"[profile] active: …\". The starter entry \
                 below is inert until you edit its match. Full reference: \
                 docs/CONFIGURATION.md → Target profiles.",
            )
            .color(palette.text_muted)
            .text_style(egui::TextStyle::Small),
        );
        ui.add_space(8.0);
        let show_profiles_help = label_with_help(
            ui,
            "Profiles JSON",
            "Advanced JSON profile definitions. Save persists valid JSON profiles into the config file.",
        );
        inline_help(
            ui,
            show_profiles_help,
            "Advanced JSON profile definitions. Save persists valid JSON profiles into the config file.",
        );
        ui.add(
            egui::TextEdit::multiline(&mut self.settings.profiles_json)
                .font(egui::TextStyle::Monospace)
                .desired_rows(14)
                .desired_width(f32::INFINITY),
        );

        ui.add_space(8.0);
        self.profiles_window_picker(ui, palette);
    }

    fn profiles_window_picker(&mut self, ui: &mut egui::Ui, palette: UiPalette) {
        ui.separator();
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("Pick target window")
                .strong()
                .color(palette.text_muted)
                .text_style(egui::TextStyle::Small),
        );
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new(
                "List open windows and insert a profile entry for the one you want to target. \
                 Windows only — window enumeration is not available on Wayland or macOS.",
            )
            .color(palette.text_muted)
            .text_style(egui::TextStyle::Small),
        );
        ui.add_space(4.0);

        if ui
            .add_enabled(
                self.background_task.is_none(),
                egui::Button::new("List open windows"),
            )
            .on_hover_text(
                "Run the worker to enumerate visible top-level windows. \
                 Windows only; exits with an error on other platforms.",
            )
            .clicked()
        {
            self.run_list_windows();
        }

        if self.window_options.is_empty() {
            return;
        }

        ui.add_space(4.0);

        let total = self.window_options.len();
        let visible = total.min(12);
        if total > visible {
            ui.label(
                egui::RichText::new(format!(
                    "Showing first {visible} of {total} windows — scroll or narrow by title."
                ))
                .color(palette.text_muted)
                .text_style(egui::TextStyle::Small),
            );
        }

        let window_options = self.window_options.clone();
        let profiles_json = &mut self.settings.profiles_json;

        egui::ScrollArea::vertical()
            .max_height(260.0)
            .id_salt("window_picker_scroll")
            .show(ui, |ui| {
                for (title, process) in window_options.iter().take(visible) {
                    ui.horizontal(|ui| {
                        let display_title = compact_label(title, 60);
                        let row_label = if process.is_empty() {
                            display_title
                        } else {
                            format!("{process} — {display_title}")
                        };
                        ui.label(egui::RichText::new(row_label).text_style(egui::TextStyle::Small));
                        if ui
                            .small_button("Insert")
                            .on_hover_text(
                                "Append a new profile object matching this window to the \
                                 Profiles JSON above. The current JSON must be a valid array.",
                            )
                            .clicked()
                        {
                            let new_json = insert_window_profile(profiles_json, title, process);
                            if let Some(updated) = new_json {
                                *profiles_json = updated;
                            }
                        }
                    });
                }
            });
    }
}

/// Append a profile entry for *(title, process)* to *profiles_json*.
///
/// Parses the current JSON, pushes a new profile object, and returns the
/// pretty-printed result.  Returns `None` when the input is not a valid JSON
/// array (the UI surfaces the existing validation error instead of
/// overwriting with partial content).
pub(in crate::ui) fn insert_window_profile(
    profiles_json: &str,
    title: &str,
    process: &str,
) -> Option<String> {
    let mut arr: Vec<serde_json::Value> = serde_json::from_str(profiles_json.trim()).ok()?;

    // Derive a readable profile name from the process stem (strip extension).
    let basename = process
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(process);
    // Strip the file extension (last `.xxx`) to get a clean name.
    let stem = if let Some(dot_pos) = basename.rfind('.') {
        &basename[..dot_pos]
    } else {
        basename
    };
    let name = if !stem.is_empty() {
        stem.to_owned()
    } else if !process.is_empty() {
        process.to_owned()
    } else {
        title
            .split_whitespace()
            .next()
            .unwrap_or("window")
            .to_owned()
    };

    let entry = serde_json::json!({
        "name": name,
        "match": {
            "process": [process],
            "title": [title]
        },
        "settings": {}
    });
    arr.push(entry);
    serde_json::to_string_pretty(&arr).ok()
}

#[cfg(test)]
mod profiles_helper_tests {
    use super::*;

    #[test]
    fn inserts_profile_into_existing_array() {
        let json = r#"[{"name":"existing","match":{"title":["Slack"]},"settings":{}}]"#;
        let result = insert_window_profile(json, "Notepad - Untitled", "notepad.exe").unwrap();
        let arr: Vec<serde_json::Value> = serde_json::from_str(&result).unwrap();
        assert_eq!(arr.len(), 2);
        let new_entry = &arr[1];
        assert_eq!(new_entry["name"], "notepad");
        let match_process = new_entry["match"]["process"].as_array().unwrap();
        assert_eq!(match_process[0], "notepad.exe");
        let match_title = new_entry["match"]["title"].as_array().unwrap();
        assert_eq!(match_title[0], "Notepad - Untitled");
        assert_eq!(new_entry["settings"], serde_json::json!({}));
    }

    #[test]
    fn inserts_into_empty_array() {
        let result = insert_window_profile("[]", "Visual Studio Code", "Code.exe").unwrap();
        let arr: Vec<serde_json::Value> = serde_json::from_str(&result).unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["match"]["process"][0], "Code.exe");
    }

    #[test]
    fn returns_none_for_invalid_json() {
        let result = insert_window_profile("not valid json", "Title", "proc.exe");
        assert!(result.is_none());
    }

    #[test]
    fn returns_none_for_non_array_json() {
        let result = insert_window_profile(r#"{"not": "an array"}"#, "Title", "proc.exe");
        assert!(result.is_none());
    }

    #[test]
    fn name_derived_from_process_stem() {
        let result = insert_window_profile("[]", "My Window", "myapp.exe").unwrap();
        let arr: Vec<serde_json::Value> = serde_json::from_str(&result).unwrap();
        // The name should be the process stem (without extension).
        let name = arr[0]["name"].as_str().unwrap();
        assert!(!name.is_empty());
    }

    #[test]
    fn empty_process_falls_back_to_title_word() {
        let result = insert_window_profile("[]", "Slack - General", "").unwrap();
        let arr: Vec<serde_json::Value> = serde_json::from_str(&result).unwrap();
        let name = arr[0]["name"].as_str().unwrap();
        assert!(!name.is_empty());
    }
}
