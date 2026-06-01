"""Optional PySide/Qt settings UI for whisper-dictate."""
from __future__ import annotations

import hashlib
import os
import locale
import subprocess
import sys
from pathlib import Path

from vp_config import SETTING_BY_KEY, config_path, effective_config, load_config, save_config, touch_reload_signal
from vp_parakeet import DEFAULT_MODEL as DEFAULT_PARAKEET_MODEL, PARAKEET_MODELS


def _missing_pyside_error() -> RuntimeError:
    return RuntimeError(
        "The settings UI requires PySide6. Install the optional UI bundle: "
        "python -m pip install -r requirements-ui.txt"
    )


def run_settings_ui() -> int:
    try:
        from PySide6.QtCore import QLockFile, QProcess, QProcessEnvironment, QTimer, Qt
        from PySide6.QtGui import QAction, QIcon, QTextCursor
        from PySide6.QtNetwork import QLocalServer, QLocalSocket
        from PySide6.QtWidgets import (
            QApplication, QCheckBox, QComboBox, QFileDialog, QFormLayout,
            QHBoxLayout, QLabel, QLineEdit, QMainWindow, QMessageBox,
            QPlainTextEdit, QPushButton, QSpinBox, QDoubleSpinBox, QSystemTrayIcon,
            QStyle, QTabWidget, QTextEdit, QToolButton, QToolTip, QVBoxLayout, QWidget,
        )
    except ImportError as exc:
        raise _missing_pyside_error() from exc

    from vp_dictionary import (
        add_dictionary_replacements,
        dictionary_target_path,
        ensure_dictionary_file,
        open_dictionary,
    )

    def activation_server_name() -> str:
        key = str(config_path().with_name("settings-ui")).lower().encode("utf-8", errors="replace")
        return "whisper-dictate-settings-" + hashlib.sha1(key).hexdigest()

    def activate_existing_ui(server_name: str) -> bool:
        socket = QLocalSocket()
        socket.connectToServer(server_name)
        if not socket.waitForConnected(500):
            return False
        socket.write(b"show\n")
        socket.flush()
        socket.waitForBytesWritten(500)
        socket.disconnectFromServer()
        return True

    def load_app_icon(app: QApplication) -> QIcon:
        here = Path(__file__).resolve().parent
        for path in (here / "whisper-dictate.ico", here / "assets" / "whisper-dictate.ico"):
            if path.exists():
                icon = QIcon(str(path))
                if not icon.isNull():
                    return icon
        return app.style().standardIcon(QStyle.StandardPixmap.SP_ComputerIcon)

    class SettingsWindow(QMainWindow):
        def __init__(self):
            super().__init__()
            self.setWindowTitle("whisper-dictate settings")
            self.resize(860, 680)
            self._controls: dict[str, object] = {}
            self._labels: dict[str, QLabel] = {}
            self._status = QLabel("")
            self._backend_note = QLabel("")
            self._settings_buttons: QWidget | None = None
            self._runtime_status = QLabel("Stopped")
            self._runtime_log = QPlainTextEdit()
            self._runtime_log.setReadOnly(True)
            self._runtime_log.setMaximumBlockCount(2000)
            self._dictionary_suggestions: list[object] = []
            self._dictionary_suggest_output = QPlainTextEdit()
            self._dictionary_suggest_output.setReadOnly(True)
            self._dictionary_suggest_output.setMaximumHeight(180)
            self._dictionary_apply_button: QPushButton | None = None
            self._capacity_output = QPlainTextEdit()
            self._capacity_output.setReadOnly(True)
            self._capacity_output.setMaximumHeight(180)
            self._runtime_proc: QProcess | None = None
            self._restart_after_stop = False
            self._quit_after_stop = False
            self._closing = False
            self._build()
            self._load()
            if os.name == "nt" and (os.environ.get("VOICEPI_UI_AUTOSTART") or "1").lower() not in (
                "0", "false", "no", "off"):
                QTimer.singleShot(0, self.start_runtime)

        class HelpButton(QToolButton):
            def enterEvent(self, event) -> None:  # noqa: N802 - Qt override
                text = self.toolTip()
                if text:
                    QToolTip.showText(
                        self.mapToGlobal(self.rect().bottomRight()),
                        text,
                        self,
                        self.rect(),
                        30000,
                    )
                super().enterEvent(event)

        def _build(self) -> None:
            tabs = QTabWidget()
            if os.name == "nt":
                tabs.addTab(self._build_runtime_tab(), "Runtime")
            tabs.addTab(self._build_core_tab(), "Core")
            tabs.addTab(self._build_quality_tab(), "Quality")
            tabs.addTab(self._build_dictionary_tab(), "Dictionary")
            tabs.addTab(self._build_output_tab(), "Output")

            self._backend_note.setWordWrap(True)
            save_btn = QPushButton("Save")
            save_btn.clicked.connect(self.save)
            reload_btn = QPushButton("Signal reload")
            reload_btn.clicked.connect(self.signal_reload)

            buttons = QHBoxLayout()
            buttons.addWidget(save_btn)
            buttons.addWidget(reload_btn)
            buttons.addStretch(1)
            buttons.addWidget(self._status)
            buttons_w = QWidget()
            buttons_w.setLayout(buttons)
            self._settings_buttons = buttons_w
            tabs.currentChanged.connect(lambda _: self._update_settings_buttons_visibility(tabs))

            root = QVBoxLayout()
            root.addWidget(self._backend_note)
            root.addWidget(tabs)
            root.addWidget(buttons_w)
            w = QWidget()
            w.setLayout(root)
            self.setCentralWidget(w)
            self._update_settings_buttons_visibility(tabs)

        def _update_settings_buttons_visibility(self, tabs: QTabWidget) -> None:
            if self._settings_buttons is None:
                return
            self._settings_buttons.setVisible(tabs.tabText(tabs.currentIndex()) != "Runtime")

        def _build_runtime_tab(self) -> QWidget:
            start_btn = QPushButton("Start")
            start_btn.clicked.connect(self.start_runtime)
            stop_btn = QPushButton("Stop")
            stop_btn.clicked.connect(self.stop_runtime)
            restart_btn = QPushButton("Restart")
            restart_btn.clicked.connect(self.restart_runtime)

            row = QHBoxLayout()
            row.addWidget(start_btn)
            row.addWidget(stop_btn)
            row.addWidget(restart_btn)
            row.addStretch(1)
            row.addWidget(self._runtime_status)

            root = QVBoxLayout()
            root.addLayout(row)
            root.addWidget(self._runtime_log)
            w = QWidget()
            w.setLayout(root)
            return w

        def _combo(self, key: str, values: list[str], editable: bool = False) -> QComboBox:
            c = QComboBox()
            c.addItems(values)
            c.setEditable(editable)
            self._controls[key] = c
            return c

        def _line(self, key: str) -> QLineEdit:
            c = QLineEdit()
            self._controls[key] = c
            return c

        def _check(self, key: str) -> QCheckBox:
            c = QCheckBox()
            self._controls[key] = c
            return c

        def _spin(self, key: str, minimum: int, maximum: int) -> QSpinBox:
            c = QSpinBox()
            c.setRange(minimum, maximum)
            self._controls[key] = c
            return c

        def _dspin(self, key: str, minimum: float, maximum: float, step: float = 0.1) -> QDoubleSpinBox:
            c = QDoubleSpinBox()
            c.setRange(minimum, maximum)
            c.setSingleStep(step)
            c.setDecimals(2)
            self._controls[key] = c
            return c

        def _build_core_tab(self) -> QWidget:
            form = QFormLayout()
            backend = self._combo("stt_backend", ["whisper", "parakeet"])
            backend.currentTextChanged.connect(lambda _: self._update_backend_controls())
            self._add_help_row(
                form, "STT backend", backend,
                "Whisper is recommended for Danish accuracy. Parakeet is experimental, very fast on NVIDIA CUDA, and autodetects language.")
            self._add_help_row(form, "Whisper model", self._combo("model", [
                "large-v3-turbo", "large-v3", "distil-large-v3", "medium", "small", "base", "tiny"
            ], editable=True), "Whisper-only model. large-v3 is most accurate; large-v3-turbo is faster.")
            self._add_help_row(
                form, "Parakeet model", self._combo("parakeet_model", PARAKEET_MODELS, editable=True),
                "Parakeet-only NVIDIA NeMo model. Use 0.6B v3 for Danish/mixed Danish-English dictation; try TDT 1.1B only for pure English quality; 0.6B v2 is a fast English-only baseline.")
            self._add_help_row(form, "Device", self._combo("device", ["auto", "cuda", "cpu"]),
                               "Compute device. Use cuda for NVIDIA GPU acceleration.")
            self._add_help_row(form, "Compute type", self._combo("compute_type", [
                "", "int8_float16", "float16", "bfloat16", "float32", "int8"
            ], editable=True), "Precision override. Usually leave empty unless debugging performance or quality.")
            self._add_help_row(
                form, "Language", self._combo("lang", ["", "da", "en", "de", "fr", "sv", "nb", "nl", "es", "it"], editable=True),
                "Whisper-only language hint. Parakeet v3 autodetects language and does not use this setting.")
            self._add_help_row(form, "Hotkey", self._line("key"), "Hold-to-talk key or chord, for example shift_l+ctrl_l.")
            capacity_btn = QPushButton("Check GPU capacity")
            capacity_btn.clicked.connect(self._check_model_capacity)
            self._add_help_row(
                form, "Model fit", capacity_btn,
                "Inspect local NVIDIA GPU free/total VRAM and show which Whisper, Parakeet and local post-processing models can run now or after freeing VRAM.")
            form.addRow("", self._capacity_output)
            return self._wrap(form)

        def _build_quality_tab(self) -> QWidget:
            form = QFormLayout()
            self._add_help_row(
                form, "Beam size", self._spin("beam_size", 1, 16),
                "Higher values can improve Whisper accuracy but increase compute time. Parakeet ignores this.")
            self._add_help_row(
                form, "Temperature ladder", self._line("temperature"),
                "Comma-separated Whisper decode temperatures. 0.0 is deterministic; fallback values can recover uncertain audio.")
            self._add_help_row(
                form, "Context min seconds", self._dspin("context_min_seconds", 0, 60, 0.5),
                "Minimum utterance length before Whisper can condition on previous text. 0 disables contextual carry-over.")
            self._add_help_row(
                form, "Parakeet min seconds", self._dspin("parakeet_min_seconds", 0, 10, 0.25),
                "Parakeet-only minimum recording length. Shorter clips are ignored because language autodetection is weaker on very short audio.")
            self._add_help_row(
                form, "Release tail ms", self._spin("release_tail_ms", 0, 1000),
                "Extra audio captured after you release the hotkey. Helps avoid clipped final syllables or words; 150-300 ms is usually enough.")
            self._add_help_row(
                form, "VAD threshold", self._dspin("vad_threshold", 0, 1, 0.05),
                "Voice activity sensitivity. Lower catches quieter speech; higher rejects more background noise.")
            self._add_help_row(
                form, "VAD min silence ms", self._spin("vad_min_silence_ms", 0, 5000),
                "Silence duration that ends a speech segment. Higher values wait longer before finalizing text.")
            self._add_help_row(
                form, "Target dBFS", self._dspin("target_dbfs", -60, 0, 1),
                "Audio normalization target volume before transcription. Less negative is louder.")
            self._add_help_row(
                form, "Min input dBFS", self._dspin("min_input_dbfs", -90, 0, 1),
                "Reject recordings below this level as too quiet.")
            self._add_help_row(
                form, "Min SNR dB", self._dspin("min_snr_db", 0, 80, 1),
                "Minimum signal-to-noise ratio. Higher values reject more noisy captures.")
            prompt = QTextEdit()
            prompt.setMaximumHeight(90)
            self._controls["initial_prompt"] = prompt
            self._add_help_row(
                form, "Initial prompt", prompt,
                "Short Whisper-only context hint. Prefer dictionary terms for product names and repeated technical words.")
            return self._wrap(form)

        def _add_help_row(self, form: QFormLayout, label: str, control: QWidget, help_text: str) -> None:
            label_w = QLabel(label)
            label_w.setToolTip(help_text)
            label_w.setStatusTip(help_text)
            help_btn = self.HelpButton()
            help_btn.setText("?")
            help_btn.setAutoRaise(True)
            help_btn.setToolTip(help_text)
            help_btn.setToolTipDuration(30000)
            help_btn.setStatusTip(help_text)
            help_btn.clicked.connect(
                lambda _checked=False, title=label, text=help_text:
                    QMessageBox.information(self, title, text)
            )
            label_row = QWidget()
            label_layout = QHBoxLayout()
            label_layout.setContentsMargins(0, 0, 0, 0)
            label_layout.addWidget(label_w)
            label_layout.addWidget(help_btn)
            label_layout.addStretch(1)
            label_row.setLayout(label_layout)
            label_row.setToolTip(help_text)
            label_row.setStatusTip(help_text)
            control.setToolTip(help_text)
            control.setStatusTip(help_text)
            for key, widget in self._controls.items():
                if widget is control:
                    self._labels[key] = label_w
                    break
            form.addRow(label_row, control)

        def _set_control_enabled(self, key: str, enabled: bool) -> None:
            control = self._controls.get(key)
            if control is not None:
                control.setEnabled(enabled)  # type: ignore[attr-defined]
            label = self._labels.get(key)
            if label is not None:
                label.setEnabled(enabled)

        def _update_backend_controls(self) -> None:
            backend_control = self._controls.get("stt_backend")
            backend = backend_control.currentText() if isinstance(backend_control, QComboBox) else "whisper"
            is_parakeet = backend == "parakeet"
            for key in (
                "model", "compute_type", "lang", "beam_size", "temperature",
                "context_min_seconds", "initial_prompt",
            ):
                self._set_control_enabled(key, not is_parakeet)
            for key in ("parakeet_model", "parakeet_min_seconds"):
                self._set_control_enabled(key, is_parakeet)
            self._backend_note.setText(
                "Parakeet is experimental and very fast on NVIDIA CUDA. It autodetects language, so Language, Whisper model, compute type, beam size, temperature and initial prompt are disabled."
                if is_parakeet else
                "Whisper is recommended for Danish accuracy. Parakeet settings are disabled while Whisper is selected."
            )

        def _build_dictionary_tab(self) -> QWidget:
            form = QFormLayout()
            row = QHBoxLayout()
            path = self._line("dictionary")
            browse = QPushButton("Browse")
            browse.clicked.connect(self._browse_dictionary)
            open_btn = QPushButton("Open")
            open_btn.clicked.connect(lambda: open_dictionary(Path(path.text()) if path.text() else None))
            row.addWidget(path)
            row.addWidget(browse)
            row.addWidget(open_btn)
            row_w = QWidget()
            row_w.setLayout(row)
            self._add_help_row(
                form, "Dictionary path", row_w,
                "JSON/text dictionary file. Terms bias Whisper within prompt limits; replacements are exact post-transcription fixes.")
            self._add_help_row(
                form, "Dictionary enabled", self._check("dictionary_enabled"),
                "Turn dictionary loading on or off without deleting the file.")
            self._add_help_row(
                form, "Max prompt terms", self._spin("dictionary_max_terms", 0, 500),
                "Maximum dictionary terms appended to Whisper's prompt. Keeps prompt injection bounded as the dictionary grows.")
            self._add_help_row(
                form, "Prompt char cap", self._spin("dictionary_prompt_chars", 0, 10000),
                "Maximum total characters from dictionary terms used in the Whisper prompt. Replacements still run even when prompt terms are capped.")
            suggest_row = QHBoxLayout()
            suggest_btn = QPushButton("Suggest replacements")
            suggest_btn.clicked.connect(self._suggest_dictionary_replacements)
            apply_btn = QPushButton("Apply suggestions")
            apply_btn.setEnabled(False)
            apply_btn.clicked.connect(self._apply_dictionary_suggestions)
            self._dictionary_apply_button = apply_btn
            suggest_row.addWidget(suggest_btn)
            suggest_row.addWidget(apply_btn)
            suggest_row.addStretch(1)
            suggest_w = QWidget()
            suggest_w.setLayout(suggest_row)
            self._add_help_row(
                form, "Benchmark suggestions", suggest_w,
                "Choose a benchmark/history JSONL file and propose deterministic replacements for repeated transcription misses. Suggestions are previewed and only written if you apply them.")
            form.addRow("", self._dictionary_suggest_output)
            return self._wrap(form)

        def _build_output_tab(self) -> QWidget:
            form = QFormLayout()
            self._add_help_row(
                form, "Inject mode", self._combo("inject_mode", ["auto", "type", "paste", "print"]),
                "How recognized text is delivered. Auto types normally, but uses paste for fragile Windows terminals and layout-sensitive punctuation.")
            self._add_help_row(
                form, "Format commands", self._combo("format_commands", ["off", "en", "da", "both"]),
                "Optional spoken formatting commands. Off is safest. English supports commands like new line, comma and period; Danish supports ny linje, komma and punktum.")
            self._add_help_row(
                form, "JSON stdout", self._check("json_output"),
                "Print one structured JSON event for each accepted utterance. Useful for automation and integration testing.")
            self._add_help_row(
                form, "Metrics JSONL", self._line("metrics_jsonl"),
                "Append one JSON object per utterance to this file, including timings, backend, model, language and injection metadata.")
            self._add_help_row(
                form, "Command hook", self._line("command_hook"),
                "Optional advanced automation command. Receives one utterance JSON event on stdin; no shell interpolation is used. Prefer JSON array form for exact arguments.")
            self._add_help_row(
                form, "Command hook timeout ms", self._spin("command_hook_timeout_ms", 100, 30000),
                "Maximum wait for the command hook. Timeout or failure is logged but never blocks or loses dictation text.")
            processor = self._combo("post_processor", ["none", "ollama"])
            processor.currentTextChanged.connect(lambda _: self._update_post_controls())
            self._add_help_row(
                form, "Post processor", processor,
                "Optional second local text pass after STT and dictionary replacements. None/raw keeps current behavior.")
            mode = self._combo("post_mode", ["raw", "clean", "prompt", "terminal", "slack", "email", "bullets"])
            mode.currentTextChanged.connect(lambda _: self._update_post_controls())
            self._add_help_row(
                form, "Post mode", mode,
                "Rewrite style for the optional second pass. Terminal mode preserves commands, paths, flags and technical terms.")
            self._add_help_row(
                form, "Post model", self._line("post_model"),
                "Local Ollama model, for example qwen2.5:3b. Smaller models are safer alongside Parakeet on 10 GB GPUs.")
            self._add_help_row(
                form, "Post base URL", self._line("post_base_url"),
                "Local Ollama URL. With Local only enabled this must be localhost.")
            self._add_help_row(
                form, "Post timeout ms", self._spin("post_timeout_ms", 100, 30000),
                "Maximum wait for local rewrite. On timeout whisper-dictate falls back to the dictionary-final text.")
            self._add_help_row(
                form, "Local only", self._check("local_only"),
                "Block cloud/BYOK providers and force Hugging Face/Transformers offline mode. Local models must already be downloaded.")
            self._add_help_row(
                form, "VOICEPI_DEBUG", self._check("debug"),
                "Print the effective startup settings before model load. Useful for verifying config/env values.")
            self._add_help_row(
                form, "VOICEPI_STT_DEBUG", self._check("stt_debug"),
                "Show raw STT backend debug output. For Parakeet this also exposes otherwise hidden NeMo startup/transcribe logs.")
            return self._wrap(form)

        def _wrap(self, layout) -> QWidget:
            w = QWidget()
            outer = QVBoxLayout()
            outer.addLayout(layout)
            outer.addStretch(1)
            w.setLayout(outer)
            return w

        def _browse_dictionary(self) -> None:
            path, _ = QFileDialog.getSaveFileName(
                self, "Dictionary JSON", str(dictionary_target_path()), "JSON (*.json);;All files (*)")
            if path:
                self._controls["dictionary"].setText(path)  # type: ignore[attr-defined]

        def _check_model_capacity(self) -> None:
            try:
                from vp_model_capacity import capacity_report

                self._capacity_output.setPlainText(capacity_report())
            except Exception as exc:  # noqa: BLE001 - surface UI errors instead of crashing
                QMessageBox.warning(self, "GPU capacity check failed", str(exc))

        def _suggest_dictionary_replacements(self) -> None:
            default_path = Path.cwd() / "benchmark" / "results.jsonl"
            path, _ = QFileDialog.getOpenFileName(
                self,
                "Benchmark or history JSONL",
                str(default_path),
                "JSONL (*.jsonl);;All files (*)",
            )
            if not path:
                return
            try:
                from vp_dictionary_suggest import suggest_replacements

                suggestions = suggest_replacements(path)
            except Exception as exc:  # noqa: BLE001 - surface UI errors instead of crashing
                self._dictionary_suggestions = []
                if self._dictionary_apply_button is not None:
                    self._dictionary_apply_button.setEnabled(False)
                QMessageBox.warning(self, "Dictionary suggestions failed", str(exc))
                return
            self._dictionary_suggestions = suggestions
            if self._dictionary_apply_button is not None:
                self._dictionary_apply_button.setEnabled(bool(suggestions))
            self._dictionary_suggest_output.setPlainText(
                self._format_dictionary_suggestions(suggestions)
            )

        def _format_dictionary_suggestions(self, suggestions: list[object]) -> str:
            if not suggestions:
                return "No dictionary replacement suggestions found."
            lines = []
            for item in suggestions:
                source = getattr(item, "source", "")
                target = getattr(item, "target", "")
                count = getattr(item, "count", 0)
                confidence = getattr(item, "confidence", 0.0)
                reason = getattr(item, "reason", "")
                lines.append(f"{source} -> {target}  ({confidence:.2f}, {count}x, {reason})")
            return "\n".join(lines)

        def _apply_dictionary_suggestions(self) -> None:
            mappings = {
                str(getattr(item, "source", "")).strip(): str(getattr(item, "target", "")).strip()
                for item in self._dictionary_suggestions
            }
            mappings = {src: dst for src, dst in mappings.items() if src and dst}
            if not mappings:
                QMessageBox.information(self, "Dictionary suggestions", "No suggestions to apply.")
                return
            control = self._controls.get("dictionary")
            raw_path = control.text().strip() if isinstance(control, QLineEdit) else ""
            path, changed = add_dictionary_replacements(
                mappings,
                Path(raw_path) if raw_path else None,
            )
            self.signal_reload(show=False)
            QMessageBox.information(
                self,
                "Dictionary suggestions applied",
                f"Added or updated {changed} replacements in:\n{path}",
            )

        def _load(self) -> None:
            values = effective_config()
            if not values.get("dictionary"):
                values["dictionary"] = str(dictionary_target_path())
            if not values.get("parakeet_model"):
                values["parakeet_model"] = DEFAULT_PARAKEET_MODEL
            for key, control in self._controls.items():
                value = values.get(key, "")
                if isinstance(control, QComboBox):
                    idx = control.findText(str(value))
                    if idx >= 0:
                        control.setCurrentIndex(idx)
                    else:
                        control.setEditText(str(value))
                elif isinstance(control, QLineEdit):
                    control.setText(str(value))
                elif isinstance(control, QCheckBox):
                    control.setChecked(str(value).lower() not in ("", "0", "false", "no", "off"))
                elif isinstance(control, QSpinBox):
                    control.setValue(int(float(value or 0)))
                elif isinstance(control, QDoubleSpinBox):
                    control.setValue(float(value or 0))
                elif isinstance(control, QTextEdit):
                    control.setPlainText(str(value))
            self._status.setText(f"Config: {config_path()}")
            self._update_backend_controls()
            self._update_post_controls()

        def _update_post_controls(self) -> None:
            processor_control = self._controls.get("post_processor")
            mode_control = self._controls.get("post_mode")
            processor = processor_control.currentText() if isinstance(processor_control, QComboBox) else "none"
            mode = mode_control.currentText() if isinstance(mode_control, QComboBox) else "raw"
            enabled = processor != "none" and mode != "raw"
            for key in ("post_model", "post_base_url", "post_timeout_ms"):
                self._set_control_enabled(key, enabled)

        def _collect(self) -> dict[str, str]:
            out: dict[str, str] = {}
            for key, control in self._controls.items():
                if isinstance(control, QComboBox):
                    value = control.currentText().strip()
                elif isinstance(control, QLineEdit):
                    value = control.text().strip()
                elif isinstance(control, QCheckBox):
                    value = "1" if control.isChecked() else "0"
                elif isinstance(control, QSpinBox):
                    value = str(control.value())
                elif isinstance(control, QDoubleSpinBox):
                    value = str(control.value())
                elif isinstance(control, QTextEdit):
                    value = control.toPlainText().strip()
                else:
                    continue
                if key == "parakeet_model" and value == DEFAULT_PARAKEET_MODEL:
                    continue
                if key in SETTING_BY_KEY and value != "":
                    out[key] = value
            return out

        def save(self) -> None:
            before = effective_config()
            data = load_config()
            collected = self._collect()
            for key in self._controls:
                if key in SETTING_BY_KEY and key not in collected:
                    data.pop(key, None)
            if "parakeet_model" not in collected and data.get("parakeet_model") == DEFAULT_PARAKEET_MODEL:
                data.pop("parakeet_model", None)
            data.update(collected)
            path = save_config(data)
            ensure_dictionary_file(Path(data["dictionary"])) if data.get("dictionary") else ensure_dictionary_file()
            after = effective_config()
            restart_keys = {s.key for s in SETTING_BY_KEY.values() if not s.live}
            changed_restart = [k for k in sorted(restart_keys) if before.get(k) != after.get(k)]
            if changed_restart and self._is_runtime_running():
                self._append_runtime_log(
                    "[ui] restart required after settings change: "
                    + ", ".join(changed_restart))
                self.restart_runtime()
            else:
                self.signal_reload(show=False)
            self._status.setText(f"Saved: {path}")

        def signal_reload(self, show: bool = True) -> None:
            path = touch_reload_signal()
            if show:
                QMessageBox.information(self, "Reload signalled", f"Runtime reload signal written:\n{path}")

        def _is_runtime_running(self) -> bool:
            return bool(self._runtime_proc and self._runtime_proc.state() != QProcess.ProcessState.NotRunning)

        def _runtime_command(self) -> tuple[str, list[str]]:
            here = Path(__file__).resolve().parent
            setup = here / "setup.ps1"
            if os.name == "nt" and setup.exists():
                return "pwsh.exe", [
                    "-NoProfile", "-ExecutionPolicy", "Bypass",
                    "-File", str(setup),
                ]
            return sys.executable, [str(here / "voice_pi.py")]

        def _append_runtime_log(self, text: str) -> None:
            self._runtime_log.appendPlainText(text.rstrip())
            self._runtime_log.moveCursor(QTextCursor.MoveOperation.End)
            self._runtime_log.ensureCursorVisible()

        def start_runtime(self) -> None:
            if self._is_runtime_running():
                return
            program, args = self._runtime_command()
            proc = QProcess(self)
            proc.setProgram(program)
            proc.setArguments(args)
            proc.setWorkingDirectory(str(Path(__file__).resolve().parent))
            proc.setProcessChannelMode(QProcess.MergedChannels)
            env = QProcessEnvironment.systemEnvironment()
            env.insert("PYTHONUNBUFFERED", "1")
            env.insert("PYTHONIOENCODING", "utf-8")
            env.insert("PIP_PROGRESS_BAR", "raw")
            env.insert("VOICEPI_MANAGED_BY_UI", "1")
            proc.setProcessEnvironment(env)
            proc.readyReadStandardOutput.connect(self._read_runtime_output)
            proc.started.connect(lambda: self._runtime_status.setText("Running"))
            proc.finished.connect(self._runtime_finished)
            self._runtime_proc = proc
            self._append_runtime_log(f"[ui] starting: {program} {' '.join(args)}")
            proc.start()

        def stop_runtime(self) -> None:
            if not self._is_runtime_running():
                self._runtime_status.setText("Stopped")
                return
            assert self._runtime_proc is not None
            self._runtime_status.setText("Stopping")
            self._runtime_proc.terminate()
            QTimer.singleShot(1000, self._kill_runtime_if_needed)

        def restart_runtime(self) -> None:
            self._restart_after_stop = True
            if self._is_runtime_running():
                self.stop_runtime()
            else:
                self.start_runtime()

        def _kill_runtime_if_needed(self) -> None:
            if self._is_runtime_running() and self._runtime_proc is not None:
                self._append_runtime_log("[ui] runtime did not stop cleanly; killing process tree")
                self._kill_runtime_tree()

        def _kill_runtime_tree(self) -> None:
            if self._runtime_proc is None:
                return
            pid = int(self._runtime_proc.processId())
            if os.name == "nt" and pid:
                subprocess.run(
                    ["taskkill", "/PID", str(pid), "/T", "/F"],
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    check=False,
                )
            else:
                self._runtime_proc.kill()

        def _read_runtime_output(self) -> None:
            if self._runtime_proc is None:
                return
            raw = bytes(self._runtime_proc.readAllStandardOutput())
            encoding = locale.getpreferredencoding(False) or "utf-8"
            try:
                data = raw.decode("utf-8")
            except UnicodeDecodeError:
                data = raw.decode(encoding, errors="replace")
            if not data:
                return
            if "Installing optional NVIDIA Parakeet dependencies" in data:
                self._runtime_status.setText("Installing Parakeet dependencies")
            elif "Setting up whisper-dictate" in data:
                self._runtime_status.setText("Installing dependencies")
            elif "model ready" in data:
                self._runtime_status.setText("Ready")
            elif "listening" in data:
                self._runtime_status.setText("Listening")
            filtered = self._filter_runtime_log(data)
            if filtered:
                self._append_runtime_log(filtered)

        def _filter_runtime_log(self, data: str) -> str:
            noisy = (
                "If you intend to do training or fine-tuning",
                "If you intend to do validation",
                "Train config :",
                "Validation config :",
                "The following configuration keys are ignored by Lhotse dataloader",
                "pretokenize=True",
                "Transcribing:",
                "Couldn't find ffmpeg or avconv",
                "triton not found; flop counting will not work",
                "Redirects are currently not supported in Windows or MacOs",
                "No exporters were provided",
                "OneLogger: Setting error_handling_strategy",
            )
            lines = []
            skipping_block = False
            for line in data.splitlines():
                stripped = line.strip()
                if any(pattern in line for pattern in noisy):
                    skipping_block = stripped.endswith(":")
                    continue
                if skipping_block:
                    if stripped.startswith("[") or stripped.startswith("W") or stripped.startswith("Traceback"):
                        skipping_block = False
                    else:
                        continue
                if stripped:
                    lines.append(line)
            return "\n".join(lines)

        def _runtime_finished(self, code: int, status) -> None:
            self._runtime_status.setText(f"Stopped ({code})" if code else "Stopped")
            self._append_runtime_log(f"[ui] runtime exited with code {code}")
            self._runtime_proc = None
            if self._quit_after_stop:
                app = QApplication.instance()
                if app is not None:
                    app.quit()
                return
            if self._restart_after_stop:
                self._restart_after_stop = False
                QTimer.singleShot(250, self.start_runtime)

        def show_and_activate(self) -> None:
            if self.isMinimized():
                self.setWindowState((self.windowState() & ~Qt.WindowState.WindowMinimized) | Qt.WindowState.WindowActive)
            self.show()
            self.raise_()
            self.activateWindow()

        def closeEvent(self, event) -> None:  # noqa: N802 - Qt override
            if self._closing:
                event.accept()
                return
            self._closing = True
            self._quit_after_stop = True
            self._restart_after_stop = False
            self.hide()
            if self._is_runtime_running():
                self.stop_runtime()
            else:
                app = QApplication.instance()
                if app is not None:
                    app.quit()
            event.ignore()

        def quit_app(self) -> None:
            self._closing = True
            self._restart_after_stop = False
            if self._is_runtime_running():
                self._quit_after_stop = True
                self.stop_runtime()
                return
            app = QApplication.instance()
            if app is not None:
                app.quit()

    app = QApplication.instance() or QApplication(sys.argv)
    app.setApplicationDisplayName("whisper-dictate")
    app.setQuitOnLastWindowClosed(False)

    lock_path = config_path().with_name("settings-ui.lock")
    server_name = activation_server_name()
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    instance_lock = QLockFile(str(lock_path))
    locked = instance_lock.tryLock(100)
    if not locked:
        if activate_existing_ui(server_name):
            return 0
        instance_lock.removeStaleLockFile()
        locked = instance_lock.tryLock(100)
    if not locked:
        QMessageBox.information(
            None,
            "whisper-dictate",
            "whisper-dictate Settings UI is already running.",
        )
        return 0

    win = SettingsWindow()
    icon = load_app_icon(app)
    if not isinstance(icon, QIcon) or icon.isNull():
        icon = win.style().standardIcon(QStyle.StandardPixmap.SP_ComputerIcon)
    app.setWindowIcon(icon)
    win.setWindowIcon(icon)
    activation_server = QLocalServer(app)
    QLocalServer.removeServer(server_name)

    def handle_activation_request() -> None:
        while activation_server.hasPendingConnections():
            conn = activation_server.nextPendingConnection()
            if conn is not None:
                conn.close()
        win.show_and_activate()

    activation_server.newConnection.connect(handle_activation_request)
    activation_server.listen(server_name)
    tray = QSystemTrayIcon(icon, app)
    tray.setToolTip("whisper-dictate")
    menu = tray.contextMenu()
    if menu is None:
        from PySide6.QtWidgets import QMenu
        menu = QMenu()
        tray.setContextMenu(menu)
    show_action = QAction("Settings", tray)
    show_action.triggered.connect(lambda: (win.show(), win.raise_(), win.activateWindow()))
    dict_action = QAction("Open dictionary", tray)
    dict_action.triggered.connect(lambda: open_dictionary())
    quit_action = QAction("Quit UI", tray)
    quit_action.triggered.connect(win.quit_app)
    menu.addAction(show_action)
    menu.addAction(dict_action)
    menu.addSeparator()
    menu.addAction(quit_action)
    tray.show()
    win.show_and_activate()
    QTimer.singleShot(250, win.show_and_activate)
    return int(app.exec())
