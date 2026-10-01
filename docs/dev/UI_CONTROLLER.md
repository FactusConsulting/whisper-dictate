# Desktop controller ownership

`ui/app.rs` owns one `WhisperDictateApp` and the stable eframe/public controller
paths. Its children separate rendering (`view`), configured-model policy
(`model_policy`), explicit start/stop/restart (`runtime_lifecycle`), polling,
and worker-event projection (`worker_events`). Capture health and status errors
are reduced separately while preserving their original ordering.

The children do not construct another UI or managed runtime. Existing tests
follow their owners, including the isolated hidden Windows event-loop test.
