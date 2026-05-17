# nixpkgs PR files

`package.nix` here is a copy of `../package.nix` adapted for submission to
[NixOS/nixpkgs](https://github.com/NixOS/nixpkgs).  The `src` argument is
removed (it uses `fetchFromGitHub` directly) and the `hash` placeholder must
be replaced with the real SRI hash before submitting.

## Getting the hash

```bash
nix store prefetch-file --unpack \
  "https://github.com/FactusConsulting/whisper-dictate/archive/refs/tags/v0.2.23.tar.gz"
```

## Placement in nixpkgs

The file belongs at:

```
pkgs/by-name/wh/whisper-dictate/package.nix
```

## Testing before PR

```bash
cd /path/to/nixpkgs
nix-build -A whisper-dictate
# or with flakes:
nix build .#whisper-dictate
```
