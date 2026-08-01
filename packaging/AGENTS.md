# Packaging guidance

## Code Review Rules

- Installer, archive, Chocolatey, winget, Nix, and Linux package changes must
  keep the Rust CLI/UI launch paths and bundled payloads consistent.
- Changes to installer files, shortcuts, bundled files, or Windows launch
  behavior require the local installer build and a report of generated
  `Output/*.exe` and `Output/*.zip` artifacts.
- Do not bundle dead compatibility runtimes, stale model backends, credentials,
  or account-bound URLs.
- Packaging scripts and installer messages must be safe under PowerShell,
  cmd.exe, and hidden launchers; keep output ASCII- or UTF-8-safe.
