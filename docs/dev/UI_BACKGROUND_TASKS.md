# UI background tasks

`ui/tasks.rs` retains task initiation and the stable controller method paths.
`tasks/dispatch.rs` handles pending/disconnected channels, completed results and
injection/error-revision policy. `tasks/benchmark.rs` owns benchmark tasks;
`tasks/cloud_checks.rs` transports failures and caught panics over the original
result channel. Receiver and probe ownership is cleared before result dispatch.

Moved tests remain alongside these boundaries. Result envelopes, lifecycle
ownership and user-visible behavior are unchanged.
