"""Headless CLI config UX for whisper-dictate: ``--setup`` + ``--export-config``.

This module is the worker-side, NO-ML config surface (phase 2 of the 1.12
config-UX wave). It NEVER imports torch / faster-whisper / numpy — it is reached
from ``runtime._run_utility_subcommands`` before the heavy ML deps load.

Two modes:

* ``run_setup`` — an interactive wizard. It reads ``settings_schema.json`` (the
  single source of truth, enriched with ``description``/``advanced``/``category``
  in phase 1), walks the BASIC settings first, then optionally the ADVANCED
  settings grouped by category, writes ``config.json`` and prints the equivalent
  PowerShell + bash env-lines.
* ``run_export`` — a non-interactive dump of the CURRENT effective config
  (config.json merged with VOICEPI_* env overrides, reusing
  ``vp_config.effective_config``) as a config.json blob + env-lines, secrets
  redacted by default.

The wizard dialog and the formatters are stream-injectable / pure so they
unit-test without touching real IO or the real config path.
"""
from __future__ import annotations

import getpass
import json
import math
import os
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, TextIO

from whisper_dictate.vp_config import (
    SETTINGS,
    SETTING_BY_KEY,
    effective_config,
    save_config,
)

# --- Secrets ------------------------------------------------------------
# API keys are never config.json keys; they live in the OS credential store
# (UI) or these env vars (headless). The wizard prompts for them and emits the
# env-line, REDACTED in printed output. These mirror the names the worker reads
# (vp_external_api / vp_postprocess) and the Rust UI's api_keys.rs.
STT_API_KEY_ENV = "VOICEPI_STT_API_KEY"
POST_API_KEY_ENV = "VOICEPI_POST_API_KEY"
SECRET_ENVS: tuple[str, ...] = (
    STT_API_KEY_ENV,
    POST_API_KEY_ENV,
    "GROQ_API_KEY",
    "OPENAI_API_KEY",
)
REDACTED = "***"

# --- Enum choices -------------------------------------------------------
# These derive from the Rust UI combo lists (src/rust/ui.rs and
# src/rust/ui/tabs/*.rs). The schema descriptions enumerate them in prose but do
# NOT yet expose a machine-readable `choices` array, so the worker has to mirror
# the Rust side here. SEE THE REPORT: ideally these move into the schema as a
# `choices` field so both sides derive from one source.
ENUM_CHOICES: dict[str, tuple[str, ...]] = {
    # Wave 8 of #348 dropped the "parakeet" entry; the wizard now only
    # offers the two remaining backends.
    "stt_backend": ("whisper", "openai"),
    "device": ("auto", "cuda", "cpu"),
    "compute_type": ("", "float32", "bfloat16", "float16", "int8_float16", "int8"),
    "inject_mode": ("auto", "type", "paste", "print"),
    "format_commands": ("off", "en", "da", "both"),
    "post_processor": ("none", "ollama", "openai", "groq"),
    "post_mode": ("raw", "clean", "prompt", "terminal", "slack", "email", "bullets"),
    "model": ("large-v3-turbo", "large-v3", "medium", "small", "base", "tiny"),
}

# When stt_backend == openai the user also picks a cloud provider; this is a
# wizard-only knob (it seeds stt_base_url) and is not itself a config key.
CLOUD_PROVIDERS: tuple[str, ...] = ("groq", "openai", "custom")
CLOUD_PROVIDER_BASE_URLS: dict[str, str] = {
    "groq": "https://api.groq.com/openai/v1",
    "openai": "https://api.openai.com/v1",
    "custom": "http://localhost:8000/v1",
}

# Category -> display title (mirrors gen_settings_docs.CATEGORY_TITLES so the
# wizard groups advanced settings the same way the docs do).
CATEGORY_TITLES: dict[str, str] = {
    "core": "Core",
    "stt-local": "Local speech-to-text",
    "stt-cloud": "Cloud speech-to-text",
    "audio": "Audio capture & voice activity",
    "postprocess": "Dictionary & post-processing",
    "injection": "Injection, hotkeys & feedback",
    "diagnostics": "Diagnostics, history & automation",
    "updates": "Update checks",
}


# --- Schema access ------------------------------------------------------

def _schema_rows() -> list[dict]:
    """The raw schema rows (with description/advanced/category/min/max)."""
    path = Path(__file__).with_name("settings_schema.json")
    data = json.loads(path.read_text(encoding="utf-8"))
    return list(data["settings"])


def _bounds(row: dict) -> tuple[float | None, float | None]:
    return row.get("min"), row.get("max")


# --- Pure formatters (unit-tested without IO) ---------------------------

def format_config_json(config: dict[str, str]) -> str:
    """Render the config mapping as the exact config.json text the worker reads.

    Keys are the schema ``key`` names (same shape the Rust UI writes), sorted in
    schema order for a stable, diff-friendly dump.
    """
    order = {s.key: i for i, s in enumerate(SETTINGS)}
    ordered = dict(
        sorted(config.items(), key=lambda kv: order.get(kv[0], len(order)))
    )
    return json.dumps(ordered, indent=2, ensure_ascii=False) + "\n"


def _env_for_key(key: str) -> str | None:
    setting = SETTING_BY_KEY.get(key)
    return setting.env if setting else None


def _ps_quote(value: str) -> str:
    """Single-quote a value for PowerShell, escaping embedded single quotes.

    Inside a PowerShell single-quoted string, a literal ``'`` must be doubled
    (``''``).  Without this, values like ``it's`` or API keys that happen to
    contain a quote would produce a syntax error when copy-pasted into a shell.
    """
    return "'" + value.replace("'", "''") + "'"


def format_powershell_lines(
    config: dict[str, str],
    secrets: dict[str, str] | None = None,
) -> str:
    """PowerShell ``$env:VOICEPI_X = '...'`` lines for the config + secrets."""
    lines: list[str] = []
    for key, value in config.items():
        env = _env_for_key(key)
        if env:
            lines.append(f"$env:{env} = {_ps_quote(value)}")
    for env, value in (secrets or {}).items():
        lines.append(f"$env:{env} = {_ps_quote(value)}")
    return "\n".join(lines) + ("\n" if lines else "")


def format_bash_lines(
    config: dict[str, str],
    secrets: dict[str, str] | None = None,
) -> str:
    """bash ``export VOICEPI_X=...`` lines for the config + secrets."""
    lines: list[str] = []
    for key, value in config.items():
        env = _env_for_key(key)
        if env:
            lines.append(f"export {env}={_bash_quote(value)}")
    for env, value in (secrets or {}).items():
        lines.append(f"export {env}={_bash_quote(value)}")
    return "\n".join(lines) + ("\n" if lines else "")


def _bash_quote(value: str) -> str:
    """Single-quote a value for bash, escaping embedded single quotes."""
    if value == "":
        return "''"
    if all(c.isalnum() or c in "-._/:" for c in value):
        return value
    return "'" + value.replace("'", "'\\''") + "'"


def format_export(
    config: dict[str, str],
    secret_envs: dict[str, str],
    *,
    include_secrets: bool,
) -> str:
    """Assemble the full ``--export-config`` output (config.json + env-lines).

    ``secret_envs`` maps secret env-var name -> its (true) value; when
    ``include_secrets`` is False they are emitted as ``***`` everywhere.
    """
    shown_secrets = {
        env: (value if include_secrets else REDACTED)
        for env, value in secret_envs.items()
    }
    parts: list[str] = []
    parts.append("# === config.json ===")
    parts.append(format_config_json(config).rstrip("\n"))
    parts.append("")
    if shown_secrets and not include_secrets:
        parts.append(
            "# secrets redacted (***) — re-run with --include-secrets to emit "
            "them in full"
        )
    parts.append("# === PowerShell ===")
    parts.append(format_powershell_lines(config, shown_secrets).rstrip("\n"))
    parts.append("")
    parts.append("# === bash ===")
    parts.append(format_bash_lines(config, shown_secrets).rstrip("\n"))
    return "\n".join(parts).rstrip("\n") + "\n"


# --- Wizard engine (stream-injectable) ----------------------------------

@dataclass
class WizardResult:
    config: dict[str, str]
    secret_envs: dict[str, str]  # env name -> value the user typed (may be "")


def _coerce_number(raw: str, row: dict) -> float | int:
    """Parse a numeric answer; raise ValueError if out of the schema bounds."""
    value = float(raw)
    if not math.isfinite(value):
        raise ValueError("must be a finite number (not nan/inf)")
    lo, hi = _bounds(row)
    if lo is not None and value < lo:
        raise ValueError(f"must be >= {lo}")
    if hi is not None and value > hi:
        raise ValueError(f"must be <= {hi}")
    # Preserve integer look when the step / bounds are integral.
    if value.is_integer() and "." not in raw:
        return int(value)
    return value


def _is_numeric(row: dict) -> bool:
    return ("min" in row) or ("max" in row)


class _Wizard:
    """Drives the interactive dialog over injected input/output callables."""

    def __init__(
        self,
        input_fn: Callable[[str], str],
        output_fn: Callable[[str], None],
        existing: dict[str, str],
    ) -> None:
        self._input = input_fn
        self._output = output_fn
        self._existing = existing
        self.config: dict[str, str] = {}
        self.secret_envs: dict[str, str] = {}
        self._cloud_provider_prompted = False

    def _say(self, text: str = "") -> None:
        self._output(text)

    def _ask(self, prompt: str) -> str:
        return self._input(prompt)

    def _current(self, row: dict) -> str | None:
        """Current effective value: existing config/env, else schema default."""
        key = row["key"]
        if key in self._existing:
            return self._existing[key]
        default = row.get("default")
        return None if default is None else str(default)

    def _prompt_one(self, row: dict) -> None:
        key = row["key"]
        current = self._current(row)
        shown = current if current not in (None, "") else "(unset)"
        choices = ENUM_CHOICES.get(key)
        self._say()
        self._say(f"{row['description']}")
        if choices:
            visible = [c if c != "" else "(auto)" for c in choices]
            self._say(f"  choices: {', '.join(visible)}")
        while True:
            answer = self._ask(f"  {key} [{shown}]: ").strip()
            if answer == "":
                # ENTER keeps current/default: only persist a non-default value.
                if current not in (None, "") and current != _default_str(row):
                    self.config[key] = current
                return
            accepted = self._validate_answer(answer, row, choices)
            if accepted is not None:
                self.config[key] = accepted
                return
            # _validate_answer already printed the reason; re-prompt.

    def _validate_answer(
        self, answer: str, row: dict, choices: tuple[str, ...] | None
    ) -> str | None:
        """Return the value to store, or None (after reporting why) to re-prompt.

        When choices contains ``""`` (e.g. compute_type "auto"), the human-
        readable token ``"auto"`` is also accepted and mapped to ``""``.
        """
        if choices is not None:
            # Map the human-readable token "auto" to "" when "" is a valid choice.
            if answer == "auto" and "" in choices:
                return ""
            if answer not in choices:
                visible = [c if c != "" else "auto" for c in choices]
                self._say(f"  ! '{answer}' is not one of: {', '.join(visible)}")
                return None
        if _is_numeric(row):
            try:
                return str(_coerce_number(answer, row))
            except ValueError as e:
                self._say(f"  ! invalid number: {e}")
                return None
        return answer

    def _maybe_prompt_secret(self, getpass_fn: Callable[[str], str]) -> None:
        """Prompt for an API key if the chosen backend/post-processor needs one."""
        backend = self.config.get("stt_backend", self._existing.get("stt_backend"))
        if backend == "openai":
            self._prompt_secret(
                STT_API_KEY_ENV,
                "Cloud STT API key (stored via env var; never written to "
                "config.json)",
                getpass_fn,
            )
        post = self.config.get("post_processor", self._existing.get("post_processor"))
        if post in ("openai", "groq"):
            self._prompt_secret(
                POST_API_KEY_ENV,
                "Cloud post-processing API key (env var; blank reuses the STT "
                "key when set)",
                getpass_fn,
            )

    def _prompt_secret(
        self, env: str, desc: str, getpass_fn: Callable[[str], str]
    ) -> None:
        self._say()
        self._say(desc)
        value = getpass_fn(f"  {env} (hidden, ENTER to skip): ").strip()
        if value:
            self.secret_envs[env] = value

    def _ask_yes_no(self, prompt: str) -> bool:
        answer = self._ask(prompt).strip().lower()
        return answer in ("y", "yes")

    def _current_provider(self) -> str:
        """Derive the current cloud-provider name from the effective stt_base_url.

        Returns "groq" (the most common entry-point) when no URL has been
        explicitly configured — i.e. when the URL is absent or matches the
        schema default, which means the user never consciously set it.
        """
        current_url = self.config.get(
            "stt_base_url", self._existing.get("stt_base_url", "")
        )
        schema_default = str(SETTING_BY_KEY["stt_base_url"].default) if "stt_base_url" in SETTING_BY_KEY else ""
        # If the URL is the schema default or absent, treat as "not configured".
        if not current_url or current_url == schema_default:
            return "groq"
        # Reverse-lookup: find a named provider whose URL matches.
        for name, url in CLOUD_PROVIDER_BASE_URLS.items():
            if name != "custom" and current_url == url:
                return name
        # URL is set but doesn't match any named provider → custom.
        return "custom"

    def _prompt_cloud_provider(self) -> None:
        """If backend=openai, seed stt_base_url from the chosen provider.

        Defaults to the CURRENT provider so ENTER-to-keep never clobbers an
        existing custom stt_base_url.  Only writes stt_base_url when the user
        actively picks a provider (or enters a custom URL).
        """
        backend = self.config.get("stt_backend", self._existing.get("stt_backend"))
        if backend != "openai" or self._cloud_provider_prompted:
            return
        self._cloud_provider_prompted = True

        default_provider = self._current_provider()
        current_url = self.config.get(
            "stt_base_url", self._existing.get("stt_base_url", "")
        )

        self._say()
        self._say("Cloud provider (seeds the API base URL):")
        self._say(f"  choices: {', '.join(CLOUD_PROVIDERS)}")
        while True:
            answer = self._ask(
                f"  cloud provider [{default_provider}]: "
            ).strip().lower()
            provider = answer or default_provider
            if provider not in CLOUD_PROVIDERS:
                self._say(f"  ! not one of: {', '.join(CLOUD_PROVIDERS)}")
                continue

            if provider == "custom":
                # Prompt for the custom URL, defaulting to the current value.
                shown_url = current_url or "http://localhost:8000/v1"
                url_answer = self._ask(
                    f"  stt_base_url [{shown_url}]: "
                ).strip()
                chosen_url = url_answer or shown_url
                self.config["stt_base_url"] = chosen_url
            else:
                chosen_url = CLOUD_PROVIDER_BASE_URLS[provider]
                # Only persist a non-default base URL (keep config.json minimal).
                default_base = SETTING_BY_KEY["stt_base_url"].default
                if chosen_url != default_base:
                    self.config["stt_base_url"] = chosen_url
                elif "stt_base_url" in self.config:
                    # User switched back to the default provider: remove override.
                    del self.config["stt_base_url"]
            return

    def run(self, *, getpass_fn: Callable[[str], str]) -> WizardResult:
        rows = _schema_rows()
        basic = [r for r in rows if not r.get("advanced", True)]
        advanced = [r for r in rows if r.get("advanced", True)]

        self._say("whisper-dictate setup — basic settings")
        self._say("Press ENTER to keep the shown value; type to change it.")
        for row in basic:
            self._prompt_one(row)

        self._prompt_cloud_provider()

        if self._ask_yes_no("\nRun advanced setup? [y/N]: "):
            for category, title in CATEGORY_TITLES.items():
                cat_rows = [r for r in advanced if r.get("category") == category]
                if not cat_rows:
                    continue
                self._say()
                self._say(f"--- {title} ---")
                for row in cat_rows:
                    self._prompt_one(row)
            # A cloud provider may have been picked only in the advanced track.
            self._prompt_cloud_provider()

        self._maybe_prompt_secret(getpass_fn)
        return WizardResult(config=self.config, secret_envs=self.secret_envs)


def _default_str(row: dict) -> str | None:
    default = row.get("default")
    return None if default is None else str(default)


# --- Export source resolution -------------------------------------------

def resolve_secret_envs(env: dict[str, str] | None = None) -> dict[str, str]:
    """Collect the API-key env vars that are actually set, for export.

    Reuses the worker's notion of where keys live (the same env names
    vp_external_api / vp_postprocess read). Only non-empty ones are returned.
    """
    source = os.environ if env is None else env
    out: dict[str, str] = {}
    for name in SECRET_ENVS:
        value = (source.get(name) or "").strip()
        if value:
            out[name] = value
    return out


# --- Public entry points ------------------------------------------------

def run_setup(
    *,
    input_fn: Callable[[str], str] | None = None,
    output_fn: Callable[[str], None] | None = None,
    getpass_fn: Callable[[str], str] | None = None,
    stdin: TextIO | None = None,
    stdout: TextIO | None = None,
    config_writer: Callable[[dict[str, str]], Path] | None = None,
    minimal: bool = True,
) -> int:
    """Run the interactive setup wizard, write config.json, print env-lines.

    Stream-injectable: tests pass ``input_fn``/``output_fn``/``getpass_fn`` (or a
    scripted ``stdin``) so no real TTY/IO is touched. ``minimal`` (default) only
    persists keys that differ from the schema default, keeping config.json lean.

    TTY-safety: with no injected ``input_fn`` and a non-TTY stdin, the wizard
    reads scripted answers from stdin line-by-line (so it can be piped/tested)
    rather than blocking on a prompt.
    """
    out = stdout or sys.stdout

    def _emit(text: str = "") -> None:
        print(text, file=out, flush=True)

    output = output_fn or _emit

    if input_fn is None:
        in_stream = stdin or sys.stdin
        is_tty = bool(getattr(in_stream, "isatty", lambda: False)())
        if is_tty:
            input_fn = input  # real interactive prompt
            # On a real TTY, hide secret input as intended.
            if getpass_fn is None:
                getpass_fn = getpass.getpass
        else:
            # Non-TTY (piped/tests): drive BOTH normal and secret prompts from
            # scripted stdin lines so nothing blocks on a missing terminal —
            # getpass.getpass would read from /dev/tty, not the pipe, and hang.
            line_iter = iter(in_stream.readlines())

            def _scripted(prompt: str) -> str:
                try:
                    return next(line_iter).rstrip("\n")
                except StopIteration:
                    # No more scripted input: treat as ENTER (keep default).
                    return ""

            input_fn = _scripted
            if getpass_fn is None:
                getpass_fn = _scripted
    if getpass_fn is None:
        getpass_fn = getpass.getpass

    existing = effective_config()
    wizard = _Wizard(input_fn, output, existing)
    result = wizard.run(getpass_fn=getpass_fn)

    config = result.config if minimal else {**existing, **result.config}
    writer = config_writer or save_config
    path = writer(config)

    output("")
    output(f"Wrote config to: {path}")
    output("")
    secret_envs = dict.fromkeys(result.secret_envs, REDACTED)
    if secret_envs:
        output(
            "# secrets are NOT written to config.json; set the env var yourself "
            "(shown redacted below)"
        )
    output("# === PowerShell ===")
    output(format_powershell_lines(config, secret_envs).rstrip("\n"))
    output("")
    output("# === bash ===")
    output(format_bash_lines(config, secret_envs).rstrip("\n"))
    return 0


def run_export(
    *,
    include_secrets: bool = False,
    output_fn: Callable[[str], None] | None = None,
    stdout: TextIO | None = None,
    env: dict[str, str] | None = None,
) -> int:
    """Dump the current effective config (config.json + env) to stdout.

    Reuses ``vp_config.effective_config`` for the merge so the dump mirrors what
    the worker actually resolves at startup. Secrets are redacted unless
    ``include_secrets`` is set.
    """
    out = stdout or sys.stdout

    def _emit(text: str = "") -> None:
        print(text, file=out, flush=True)

    output = output_fn or _emit
    config = effective_config()
    secret_envs = resolve_secret_envs(env)
    output(format_export(config, secret_envs, include_secrets=include_secrets).rstrip("\n"))
    return 0
