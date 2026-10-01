# UI model-state ownership

`ui/whisper_models_state.rs` owns download status and the shared state/cache.
Its `verification` child owns non-blocking verification, cached availability
and stale file-fingerprint protection. There is one state/cache owner; the
extraction does not introduce another downloader or change model selection.

Existing download, cancellation, hash/cache and replacement-race tests follow
the module boundary; the public state API remains stable.
