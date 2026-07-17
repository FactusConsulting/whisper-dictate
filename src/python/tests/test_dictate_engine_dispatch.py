"""Tests for the ``VOICEPI_DICTATE_ENGINE`` dispatch — audit item 5
Phase A step 2.

Covers three axes:

1. :mod:`whisper_dictate.vp_dictate_engine` in isolation (env
   parsing, ready-event pinning, subprocess seams, fallback rules).
2. :func:`whisper_dictate.runtime._dispatch_engine` end-to-end
   (default path constructs :class:`Dictate`, ``rust`` path calls
   :func:`run_rust_engine`, an unknown value warns + falls back).
3. The failed-opt-in guarantee: when the Rust engine cannot start we
   run the Python engine — the worker is never dead-in-the-water.
"""
from __future__ import annotations

import io
import json
import os
import subprocess
from types import SimpleNamespace

import pytest

from whisper_dictate import runtime, vp_dictate_engine


# --------------------------------------------------------------------
# select_engine / is_known_engine
# --------------------------------------------------------------------


def test_select_engine_default_is_python():
    assert vp_dictate_engine.select_engine({}) == vp_dictate_engine.ENGINE_PYTHON


def test_select_engine_empty_string_is_python():
    assert vp_dictate_engine.select_engine(
        {vp_dictate_engine.ENGINE_ENV: "   "}
    ) == vp_dictate_engine.ENGINE_PYTHON


def test_select_engine_rust():
    assert vp_dictate_engine.select_engine(
        {vp_dictate_engine.ENGINE_ENV: "RUST"}
    ) == vp_dictate_engine.ENGINE_RUST


def test_select_engine_unknown_falls_back_to_python():
    assert vp_dictate_engine.select_engine(
        {vp_dictate_engine.ENGINE_ENV: "wgpu"}
    ) == vp_dictate_engine.ENGINE_PYTHON


def test_is_known_engine_flags_unknown():
    assert vp_dictate_engine.is_known_engine("python")
    assert vp_dictate_engine.is_known_engine("RUST")
    assert vp_dictate_engine.is_known_engine("")
    assert vp_dictate_engine.is_known_engine(None)
    assert not vp_dictate_engine.is_known_engine("wgpu")


# --------------------------------------------------------------------
# resolve_whisper_dictate_binary
# --------------------------------------------------------------------


def test_resolve_binary_prefers_env_hint(monkeypatch, tmp_path):
    hint = str(tmp_path / "whisper-dictate")
    monkeypatch.setenv("VOICEPI_RUST_INJECTOR", hint)
    assert vp_dictate_engine.resolve_whisper_dictate_binary() == hint


def test_resolve_binary_falls_back_to_which(monkeypatch):
    monkeypatch.delenv("VOICEPI_RUST_INJECTOR", raising=False)
    monkeypatch.setattr(
        vp_dictate_engine.shutil, "which",
        lambda name: "/opt/whisper-dictate" if name == "whisper-dictate" else None,
    )
    assert vp_dictate_engine.resolve_whisper_dictate_binary() == "/opt/whisper-dictate"


def test_resolve_binary_returns_none_when_missing(monkeypatch):
    monkeypatch.delenv("VOICEPI_RUST_INJECTOR", raising=False)
    monkeypatch.setattr(vp_dictate_engine.shutil, "which", lambda name: None)
    assert vp_dictate_engine.resolve_whisper_dictate_binary() is None


# --------------------------------------------------------------------
# _is_ready_event
# --------------------------------------------------------------------


def test_ready_event_valid():
    line = json.dumps({
        "kind": "ready",
        "ready": True,
        "engine": "rust",
        "chord": "ctrl_l+shift_l",
        "driver": "rdev",
    })
    assert vp_dictate_engine._is_ready_event(line)


def test_ready_event_wrong_engine_rejected():
    line = json.dumps({"kind": "ready", "ready": True, "engine": "python"})
    assert not vp_dictate_engine._is_ready_event(line)


def test_ready_event_ready_false_rejected():
    line = json.dumps({"kind": "ready", "ready": False, "engine": "rust"})
    assert not vp_dictate_engine._is_ready_event(line)


def test_ready_event_invalid_json_rejected():
    assert not vp_dictate_engine._is_ready_event("not json at all")
    assert not vp_dictate_engine._is_ready_event("[1,2,3]")
    assert not vp_dictate_engine._is_ready_event("")


def test_ready_event_stdout_line_after_ready_is_forwarded_verbatim():
    # An arbitrary worker event that Python must forward after READY.
    line = '{"kind":"worker","event":"utterance","state":"done"}'
    assert not vp_dictate_engine._is_ready_event(line)


# --------------------------------------------------------------------
# _build_dictate_run_args
# --------------------------------------------------------------------


def test_build_args_default_no_config():
    args = vp_dictate_engine._build_dictate_run_args(
        "/opt/whisper-dictate", None, json_events=True,
    )
    assert args == ["/opt/whisper-dictate", "dictate-run", "--json-events"]


def test_build_args_with_config():
    args = vp_dictate_engine._build_dictate_run_args(
        "/opt/whisper-dictate", "/etc/wd.json", json_events=True,
    )
    assert args == [
        "/opt/whisper-dictate", "dictate-run", "--json-events",
        "--config", "/etc/wd.json",
    ]


# --------------------------------------------------------------------
# run_rust_engine — failure modes
# --------------------------------------------------------------------


def test_run_rust_engine_missing_binary_returns_false(monkeypatch, capsys):
    monkeypatch.delenv("VOICEPI_RUST_INJECTOR", raising=False)
    monkeypatch.setattr(vp_dictate_engine.shutil, "which", lambda name: None)

    ran, code = vp_dictate_engine.run_rust_engine()

    assert ran is False
    assert code is None
    err = capsys.readouterr().err
    assert "binary not found" in err
    assert "falling back to python engine" in err


def test_run_rust_engine_spawn_error_returns_false(monkeypatch, capsys):
    monkeypatch.setenv("VOICEPI_RUST_INJECTOR", "/nonexistent/whisper-dictate")

    def boom(args):
        raise FileNotFoundError(2, "No such file", args[0])

    ran, code = vp_dictate_engine.run_rust_engine(_spawn=boom)

    assert ran is False
    assert code is None
    err = capsys.readouterr().err
    assert "subprocess spawn failed" in err
    assert "falling back to python engine" in err


# --------------------------------------------------------------------
# run_rust_engine — success / forwarding
# --------------------------------------------------------------------


class _FakeProc:
    """Minimal Popen-shape for the tests: exposes stdout iteration + wait."""

    def __init__(self, stdout_lines, exit_code=0):
        # Popen(stdout=PIPE, text=True, bufsize=1) yields str lines when
        # iterated; mimic that with an in-memory list.
        self.stdout = iter(stdout_lines)
        self._exit_code = exit_code
        self.terminated = False
        self.killed = False

    def wait(self, timeout=None):
        return self._exit_code

    def terminate(self):
        self.terminated = True

    def kill(self):
        self.killed = True


def test_run_rust_engine_forwards_ready_and_events(monkeypatch, capsys):
    monkeypatch.setenv("VOICEPI_RUST_INJECTOR", "/opt/whisper-dictate")
    ready = json.dumps({"kind": "ready", "ready": True, "engine": "rust"})
    event1 = '{"kind":"worker","event":"utterance"}'
    event2 = '{"kind":"shutdown","reason":"ctrl-c"}'

    def spawn(args):
        # Verify we spawn with the right args
        assert args[0] == "/opt/whisper-dictate"
        assert "dictate-run" in args
        assert "--json-events" in args
        return _FakeProc([ready + "\n", event1 + "\n", event2 + "\n"], exit_code=0)

    forwarded = []
    ran, code = vp_dictate_engine.run_rust_engine(
        _spawn=spawn,
        _stdout_sink=forwarded.append,
    )

    assert ran is True
    assert code == 0
    # Every line — including the READY envelope — is forwarded verbatim
    # so the supervisor above sees the exact same stream.
    assert forwarded == [ready, event1, event2]
    err = capsys.readouterr().err
    assert "ready-signal received" in err


def test_run_rust_engine_exit_without_ready_is_fallback(monkeypatch, capsys):
    """Rust child exits with a non-zero code BEFORE the READY signal —
    treated as a startup failure, caller falls back to Python."""
    monkeypatch.setenv("VOICEPI_RUST_INJECTOR", "/opt/whisper-dictate")
    err_line = '{"kind":"error","message":"features not compiled"}'

    def spawn(args):
        return _FakeProc([err_line + "\n"], exit_code=1)

    ran, code = vp_dictate_engine.run_rust_engine(_spawn=spawn)

    assert ran is False
    # We still surface the exit code so the caller can log it.
    assert code == 1
    err = capsys.readouterr().err
    assert "exited without READY signal" in err
    assert "falling back to python engine" in err


def test_run_rust_engine_config_path_forwarded(monkeypatch):
    monkeypatch.setenv("VOICEPI_RUST_INJECTOR", "/opt/whisper-dictate")
    seen_args = {}

    def spawn(args):
        seen_args["args"] = args
        return _FakeProc([
            json.dumps({"kind": "ready", "ready": True, "engine": "rust"}) + "\n",
        ], exit_code=0)

    vp_dictate_engine.run_rust_engine(
        config_path="/tmp/custom.json",
        _spawn=spawn,
        _stdout_sink=lambda line: None,
    )

    assert "--config" in seen_args["args"]
    idx = seen_args["args"].index("--config")
    assert seen_args["args"][idx + 1] == "/tmp/custom.json"


def test_run_rust_engine_ignores_blank_stdout_lines(monkeypatch):
    monkeypatch.setenv("VOICEPI_RUST_INJECTOR", "/opt/whisper-dictate")
    ready = json.dumps({"kind": "ready", "ready": True, "engine": "rust"})
    forwarded = []

    def spawn(args):
        return _FakeProc(["\n", ready + "\n", "\n", "\n"], exit_code=0)

    ran, _ = vp_dictate_engine.run_rust_engine(
        _spawn=spawn, _stdout_sink=forwarded.append,
    )
    assert ran is True
    assert forwarded == [ready]


# --------------------------------------------------------------------
# runtime._dispatch_engine end-to-end
# --------------------------------------------------------------------


class _RecordingDictate:
    """Stand-in for :class:`whisper_dictate.vp_dictate.Dictate`. Records
    the constructor args + whether ``run()`` was called."""

    instances: list = []

    def __init__(self, *args, **kwargs):
        self.args = args
        self.kwargs = kwargs
        self.ran = False
        _RecordingDictate.instances.append(self)

    def run(self):
        self.ran = True


@pytest.fixture
def dictate_stub(monkeypatch):
    _RecordingDictate.instances = []
    monkeypatch.setattr(runtime, "Dictate", _RecordingDictate)
    return _RecordingDictate


def _min_args():
    return SimpleNamespace(
        key="ctrl_r", mode="hold-to-talk", json=False,
        audio_source="sounddevice",
    )


def test_dispatch_default_runs_python_engine(monkeypatch, dictate_stub):
    monkeypatch.delenv(vp_dictate_engine.ENGINE_ENV, raising=False)
    monkeypatch.delenv("VOICEPI_METRICS_JSONL", raising=False)

    runtime._dispatch_engine(
        _min_args(), model=object(), lang="en", backend="faster",
        dev="cpu", ctype="int8",
        loaded_model_name="tiny.en", model_load_s=0.1,
    )

    assert len(dictate_stub.instances) == 1
    assert dictate_stub.instances[0].ran is True


def test_dispatch_python_env_runs_python_engine(monkeypatch, dictate_stub):
    monkeypatch.setenv(vp_dictate_engine.ENGINE_ENV, "python")

    runtime._dispatch_engine(
        _min_args(), model=object(), lang="en", backend="faster",
        dev="cpu", ctype="int8",
        loaded_model_name="tiny.en", model_load_s=0.1,
    )

    assert dictate_stub.instances[0].ran is True


def test_dispatch_rust_env_calls_run_rust_engine(monkeypatch, dictate_stub):
    monkeypatch.setenv(vp_dictate_engine.ENGINE_ENV, "rust")
    calls = []

    def fake_run(config_path=None):
        calls.append(config_path)
        return (True, 0)

    monkeypatch.setattr(vp_dictate_engine, "run_rust_engine", fake_run)

    with pytest.raises(SystemExit) as exc:
        runtime._dispatch_engine(
            _min_args(), model=object(), lang="en", backend="faster",
            dev="cpu", ctype="int8",
            loaded_model_name="tiny.en", model_load_s=0.1,
        )

    assert exc.value.code == 0
    assert len(calls) == 1
    # Python engine must NOT have started when the Rust engine ran.
    assert dictate_stub.instances == []


def test_dispatch_rust_env_forwards_config_env(monkeypatch, dictate_stub):
    monkeypatch.setenv(vp_dictate_engine.ENGINE_ENV, "rust")
    monkeypatch.setenv("VOICEPI_CONFIG", "/tmp/custom-config.json")
    calls = []

    def fake_run(config_path=None):
        calls.append(config_path)
        return (True, 3)

    monkeypatch.setattr(vp_dictate_engine, "run_rust_engine", fake_run)

    with pytest.raises(SystemExit) as exc:
        runtime._dispatch_engine(
            _min_args(), model=object(), lang="en", backend="faster",
            dev="cpu", ctype="int8",
            loaded_model_name="tiny.en", model_load_s=0.1,
        )
    assert exc.value.code == 3
    assert calls == ["/tmp/custom-config.json"]


def test_dispatch_rust_engine_failure_falls_back_to_python(
    monkeypatch, dictate_stub, capsys,
):
    """The load-bearing safety property: Rust engine failure MUST NOT
    take down the worker."""
    monkeypatch.setenv(vp_dictate_engine.ENGINE_ENV, "rust")

    def fake_run(config_path=None):
        return (False, None)

    monkeypatch.setattr(vp_dictate_engine, "run_rust_engine", fake_run)

    runtime._dispatch_engine(
        _min_args(), model=object(), lang="en", backend="faster",
        dev="cpu", ctype="int8",
        loaded_model_name="tiny.en", model_load_s=0.1,
    )

    # Python engine took over after Rust reported startup failure.
    assert len(dictate_stub.instances) == 1
    assert dictate_stub.instances[0].ran is True


def test_dispatch_unknown_engine_warns_and_falls_back(
    monkeypatch, dictate_stub, capsys,
):
    monkeypatch.setenv(vp_dictate_engine.ENGINE_ENV, "wgpu")

    runtime._dispatch_engine(
        _min_args(), model=object(), lang="en", backend="faster",
        dev="cpu", ctype="int8",
        loaded_model_name="tiny.en", model_load_s=0.1,
    )

    assert dictate_stub.instances[0].ran is True
    err = capsys.readouterr().err
    assert "Unknown" in err
    assert vp_dictate_engine.ENGINE_ENV in err
