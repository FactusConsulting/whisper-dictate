"""Optional PySide/Qt settings UI for whisper-dictate."""
from __future__ import annotations

import hashlib
import os
import locale
import sys
from pathlib import Path

from vp_config import SETTING_BY_KEY, config_path, effective_config, load_config, save_config, touch_reload_signal
from vp_parakeet import DEFAULT_MODEL as DEFAULT_PARAKEET_MODEL


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
            QStyle, QTabWidget, QTextEdit, QVBoxLayout, QWidget,
        )
    except ImportError as exc:
        raise _missing_pyside_error() from exc

    from vp_dictionary import dictionary_target_path, ensure_dictionary_file, open_dictionary

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
            self._status = QLabel("")
            self._runtime_status = QLabel("Stopped")
            self._runtime_log = QPlainTextEdit()
            self._runtime_log.setReadOnly(True)
            self._runtime_log.setMaximumBlockCount(2000)
            self._runtime_proc: QProcess | None = None
            self._restart_after_stop = False
            self._quit_after_stop = False
            self._build()
            self._load()
            if os.name == "nt" and (os.environ.get("VOICEPI_UI_AUTOSTART") or "1").lower() not in (
                "0", "false", "no", "off"):
                QTimer.singleShot(0, self.start_runtime)

        def _build(self) -> None:
            tabs = QTabWidget()
            if os.name == "nt":
                tabs.addTab(self._build_runtime_tab(), "Runtime")
            tabs.addTab(self._build_core_tab(), "Core")
            tabs.addTab(self._build_quality_tab(), "Quality")
            tabs.addTab(self._build_dictionary_tab(), "Dictionary")
            tabs.addTab(self._build_output_tab(), "Output")

            save_btn = QPushButton("Save")
            save_btn.clicked.connect(self.save)
            reload_btn = QPushButton("Signal reload")
            reload_btn.clicked.connect(self.signal_reload)

            buttons = QHBoxLayout()
            buttons.addWidget(save_btn)
            buttons.addWidget(reload_btn)
            buttons.addStretch(1)
            buttons.addWidget(self._status)

            root = QVBoxLayout()
            root.addWidget(tabs)
            root.addLayout(buttons)
            w = QWidget()
            w.setLayout(root)
            self.setCentralWidget(w)

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
            form.addRow("STT backend", self._combo("stt_backend", ["whisper", "parakeet"]))
            form.addRow("Whisper model", self._combo("model", [
                "large-v3-turbo", "large-v3", "distil-large-v3", "medium", "small", "base", "tiny"
            ], editable=True))
            form.addRow("Parakeet model", self._line("parakeet_model"))
            form.addRow("Device", self._combo("device", ["auto", "cuda", "cpu"]))
            form.addRow("Compute type", self._combo("compute_type", [
                "", "int8_float16", "float16", "bfloat16", "float32", "int8"
            ], editable=True))
            form.addRow("Language", self._combo("lang", ["", "da", "en", "de", "fr", "sv", "nb", "nl", "es", "it"], editable=True))
            form.addRow("Hotkey", self._line("key"))
            return self._wrap(form)

        def _build_quality_tab(self) -> QWidget:
            form = QFormLayout()
            form.addRow("Beam size", self._spin("beam_size", 1, 16))
            form.addRow("Temperature ladder", self._line("temperature"))
            form.addRow("Context min seconds", self._dspin("context_min_seconds", 0, 60, 0.5))
            form.addRow("VAD threshold", self._dspin("vad_threshold", 0, 1, 0.05))
            form.addRow("VAD min silence ms", self._spin("vad_min_silence_ms", 0, 5000))
            form.addRow("Target dBFS", self._dspin("target_dbfs", -60, 0, 1))
            form.addRow("Min input dBFS", self._dspin("min_input_dbfs", -90, 0, 1))
            form.addRow("Min SNR dB", self._dspin("min_snr_db", 0, 80, 1))
            prompt = QTextEdit()
            prompt.setMaximumHeight(90)
            self._controls["initial_prompt"] = prompt
            form.addRow("Initial prompt", prompt)
            return self._wrap(form)

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
            form.addRow("Dictionary path", row)
            form.addRow("Dictionary enabled", self._check("dictionary_enabled"))
            form.addRow("Max prompt terms", self._spin("dictionary_max_terms", 0, 500))
            form.addRow("Prompt char cap", self._spin("dictionary_prompt_chars", 0, 10000))
            return self._wrap(form)

        def _build_output_tab(self) -> QWidget:
            form = QFormLayout()
            form.addRow("Inject mode", self._combo("inject_mode", ["auto", "type", "paste", "print"]))
            form.addRow("JSON stdout", self._check("json_output"))
            form.addRow("Metrics JSONL", self._line("metrics_jsonl"))
            form.addRow("VOICEPI_DEBUG", self._check("debug"))
            form.addRow("VOICEPI_STT_DEBUG", self._check("stt_debug"))
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
            env.insert("PIP_PROGRESS_BAR", "off")
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
            QTimer.singleShot(3000, self._kill_runtime_if_needed)

        def restart_runtime(self) -> None:
            self._restart_after_stop = True
            if self._is_runtime_running():
                self.stop_runtime()
            else:
                self.start_runtime()

        def _kill_runtime_if_needed(self) -> None:
            if self._is_runtime_running() and self._runtime_proc is not None:
                self._append_runtime_log("[ui] runtime did not stop cleanly; killing process")
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
            self._append_runtime_log(data)

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
            self.hide()
            event.ignore()

        def quit_app(self) -> None:
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
