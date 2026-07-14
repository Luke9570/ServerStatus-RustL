# Offline Node Retention and Alert Delivery Design

## Goal

Fix the lifecycle conflict that removes dynamically connected servers before they can become visibly offline or trigger an offline alert. A server that has reported successfully must remain visible as offline across process and container restarts until an administrator deletes it.

The same change will integrate every inherited notification backend into the current admin workflow and audit delivery failures so they cannot silently pass as successful. The admin will support Telegram, Bark, WeChat, Email, structured Webhook receivers, and local notification logs. This work does not add SSH, remote shell, command dispatch, task execution, or any other remote-control capability.

## Confirmed Root Causes

1. Dynamic servers are removed from `hosts_map` and `stat_map` by `group_gc` before offline rule evaluation. With the default `group_gc = 30`, `offline_threshold = 30`, and a minimum alert duration of 30 seconds, the server normally disappears before the rule can fire.
2. `should_publish_stat` currently ignores `deleted_hosts`, so saving a server deletion can leave an old runtime row on the public page until another unrelated operation rebuilds state.
3. Alert evaluation state exists only inside the timer thread. A server restart forgets the last delivery time, while deleted rules and servers can leave stale in-memory entries.
4. HTTP notification implementations generally treat any received response as success. Non-2xx responses and provider-level error payloads can therefore produce no user notification while logs appear successful.
5. Several notification paths use `unwrap` or `panic` for invalid configuration, template/script compilation, SMTP construction, or log-file access. A bad notification configuration must not terminate the notification worker or prevent the server from starting.

## Chosen Architecture

Use a dedicated, versioned `runtime-state.json` in the server working directory. Docker Compose already mounts the repository `runtime/` directory at `/data`, so the file survives container replacement and remains excluded from Git.

Do not place runtime telemetry in `admin-overrides.json`. That file remains user configuration. Keep the existing `stats.json` format for compatibility and traffic counters; it is not suitable as the canonical registry because `HostStat` deliberately has asymmetric serialization fields.

The runtime state contains only:

- A schema version.
- Known server identity and public display metadata: ID, alias, group ID, location, type, public labels, weight, expiry data, notification flags, and last report timestamp.
- Per-server/per-rule alert timing: condition start and last queue-acceptance time (`last_enqueued_at`).

It must never contain agent/group passwords, admin credentials, JWT secrets, Telegram tokens, Bark device keys, SMTP passwords, webhook credentials, or private notes.

Writes use a temporary file followed by an atomic rename. A corrupt or unsupported state file is logged and ignored instead of preventing startup.

## Admin Notification Integration

All notifier objects are registered during server startup. Each notifier reads its effective base-plus-admin configuration when it handles an event, so enabling, disabling, or editing a method in the admin takes effect without restarting the server or Docker container. Disabled notifiers return without performing I/O.

The notification page keeps one independent settings block per method. Each block has its own test and save controls in the upper-right corner. Save remains disabled when the current form matches the last backend-confirmed state. A successful response updates the local baseline; a failed response restores the actionable state and shows the backend error beside the same block.

Alert-rule notification choices are generated from methods that are currently enabled and sufficiently configured. If none are available, the rule editor shows the existing configure-a-notification warning instead of silently selecting every inherited backend.

### Sensitive fields

Admin API responses expose only configured-state flags and masks for secrets. They never return secret plaintext. Saving a mask preserves the previous value; an explicit clear action removes it.

Sensitive fields are:

- Telegram bot token and chat ID.
- Bark device key.
- WeChat corporate secret.
- Email SMTP password.
- Webhook URL, Basic Auth password, and every header value.

WeChat corporate ID and agent ID, Email SMTP host, username, recipients, and non-secret labels remain visible. Logs and API errors apply the same redaction rules.

### WeChat

The admin exposes enabled state, corporate ID, corporate secret, agent ID, title, and online, offline, expiry, and health templates. Test performs both token acquisition and message delivery and returns success only when WeChat reports a successful provider code.

### Email

The admin exposes enabled state, SMTP relay, username, password, recipients, subject, title, and online, offline, expiry, and health templates. Recipients support the existing multi-address syntax. Save validates addresses and relay configuration without contacting the provider; test performs an actual SMTP delivery.

### Structured Webhook

The admin supports multiple named receivers. A receiver contains enabled state, URL, timeout, request headers, optional Basic Auth, and a MiniJinja body template. Requests remain `POST`; a header can set the content type. The admin does not expose Rhai or any other server-side script editor.

Existing `config.toml` Rhai receivers continue working unchanged until structured Webhook settings are first saved in the admin. Once an admin Webhook override exists, its structured receiver list becomes authoritative and replaces the base receiver list, preventing duplicate delivery. Clearing the admin override returns control to `config.toml`.

Webhook templates receive the existing `event`, `host`, `config`, `ip_info`, and `sys_info` context. Template compilation is validated during save. One invalid receiver rejects that module save without changing the last working backend configuration.

### Local log

The admin exposes enabled state and a message template. Admin-managed logs always write beneath `notifications/` in the server working directory; the form cannot set an arbitrary filesystem path. Existing `config.toml` log paths remain compatible until an admin log override is saved. Test writes one record and returns the relative log filename.

## Server Lifecycle

### Successful report

After authentication and configuration overrides succeed, the server is added or refreshed in the runtime registry. A newly discovered server causes an immediate state write; subsequent metadata changes use the existing periodic save path.

Servers in the `default` access group and all other valid dynamic groups follow the same lifecycle. `group_gc` remains accepted for configuration compatibility but no longer removes servers that have completed a valid report.

### Offline transition

When `last_report + offline_threshold` is exceeded, `online4` and `online6` become false. The server remains in `stat_map`, remains in the public JSON response, and is sorted below online servers.

Offline alert duration is measured from the last successful report. A rule with empty server and server-group selectors applies to all servers. A rule with Bark selected dispatches only to Bark; the same filtering applies consistently to every supported method.

### Restart recovery

At startup, known servers are restored before the first public response is published. Restored servers always start offline. Current admin overrides are then applied so renamed servers, weights, expiry settings, and manual location/type remain current.

The saved `last_enqueued_at` timestamp prevents a restart from immediately duplicating an alert within its repeat interval. If no alert was previously queued and the configured duration has elapsed, the restored offline server remains eligible for notification.

When `runtime-state.json` does not exist, the server performs a one-time best-effort import of recoverable identity and display fields from the legacy `stats.json`, then writes the new state format. Invalid legacy rows are skipped individually.

### Administrative deletion

Saving a server deletion must immediately:

- Remove it from the public response and runtime maps.
- Remove it from `runtime-state.json`.
- Remove all alert state keys for that server.
- Keep the deleted marker until a valid report with the same ID arrives or the administrator permanently clears the deleted record.

A later authenticated report with the same ID clears the deleted marker and registers the server as new, preserving the existing reconnection behavior. An unauthenticated report cannot restore it.

## Alert Evaluation

Alert state moves from a timer-local map into a small synchronized runtime-state component owned by `StatsMgr`. Evaluation remains in the existing timer thread; this is not a scheduler rewrite.

Rules behave as follows:

- Empty server and server-group selectors mean all known servers.
- Offline rules use last report time and do not require the server to remain online long enough for a separate state timer.
- Resource rules evaluate only online servers and reset their condition timer when the metric returns below threshold.
- Repeated alerts respect `repeat_interval` across process restarts.
- Deleting or disabling a rule removes alert states that no active rule can reference.
- Deleting a server removes all states for that server.
- Changing a rule ID is treated as deleting the old rule and creating a new rule.

The dispatch timestamp records only that an event was accepted by the notifier queue, not guaranteed provider delivery. HTTP providers make at most three attempts, with delays of one and three seconds, for transport errors and HTTP `408`, `429`, or `5xx` responses. Other `4xx` responses are permanent failures. This design does not convert the notifier trait to async or introduce a durable external job queue.

## Notification Backend Audit

### Shared dispatcher

The dispatcher logs a structured error containing notifier kind, event kind, and server ID whenever `notify` returns an error. It must continue processing other enabled notification methods.

Unknown legacy notification-group IDs fail closed instead of silently allowing every notifier. An empty notification group continues to mean no group restriction. Explicit notification methods continue to take precedence over the legacy group field.

Every inherited notifier is present in the dispatcher regardless of its base-file enabled state. Effective runtime configuration decides whether it sends. A configuration save therefore does not require rebuilding the notifier list.

### Telegram and Bark

- Require enabled state and complete credentials before dispatch.
- Treat only successful HTTP/provider responses as delivery success.
- Log bounded, redacted response details for non-success replies.
- Never include bot tokens, chat IDs, device keys, or credential-bearing URLs in logs.
- Apply the shared three-attempt retry policy; do not retry validation errors or permanent `4xx` responses.

### WeChat

- Validate both the token response and message-send response at HTTP and provider-code levels.
- Redact `corp_secret` and access tokens from all errors and URLs.
- Handle malformed JSON and missing tokens as errors without panicking.
- Apply the shared three-attempt retry policy to transport failures, retryable HTTP responses, and documented transient provider responses.
- Read the effective admin configuration for every event and test.

### Webhook

- Legacy invalid receiver scripts are logged and that receiver is disabled instead of crashing server startup.
- Admin-created receivers use validated MiniJinja templates and do not execute Rhai.
- Non-2xx responses are failures. Logs include the status but not the response body, which may contain third-party secrets.
- Basic-auth values and sensitive headers are never logged.
- One receiver failure does not prevent other receivers from running.

### Email

- Invalid sender, recipient, SMTP relay, or message construction returns a logged error instead of panicking.
- SMTP delivery errors remain isolated to the email notifier.
- Passwords and full credential structures are never logged.
- Read the effective admin configuration for every event and test.

### Log notifier

- Directory creation and asynchronous file errors are logged instead of panicking.
- A log-write failure does not terminate the notification worker.
- Admin-managed paths remain inside the server working directory.

## Error Handling and Compatibility

- Failure to read or write runtime state is visible in logs but does not stop live reporting.
- Runtime-state writes never expose secrets and the file remains ignored by Git.
- Existing `config.toml`, `admin-overrides.json`, public JSON, Agent protocol, reverse-proxy paths, and notification templates remain compatible.
- Existing Telegram and Bark admin settings remain compatible with their current stored format.
- Adding WeChat, Email, Webhook, or log admin fields is backward compatible because absent overrides continue to use `config.toml`.
- `group_gc` remains parseable so existing deployments do not fail configuration loading; documentation will mark its old deletion behavior as deprecated.
- The public page needs no structural redesign. It receives the retained offline row through the existing `/json/stats.json` data path.

## Test Strategy

Development follows red-green-refactor. Required regression coverage:

1. A valid dynamic server survives beyond `group_gc`, becomes offline, and remains in the public response.
2. An unrestricted offline rule applies to a dynamic server and emits only the selected Bark method.
3. Offline alert duration and repeat interval work before and after runtime-state reload.
4. Runtime recovery restores servers as offline and filters deleted IDs.
5. Administrative deletion removes public, runtime, persisted, and alert state; a later authenticated report with the same ID can re-register it.
6. Disabled/deleted rules prune stale alert states.
7. Legacy `stats.json` import skips malformed rows and never imports secrets.
8. Telegram, Bark, WeChat, and Webhook distinguish successful, retryable, and permanent failure responses with redacted diagnostics.
9. Invalid Email, legacy Webhook, structured Webhook, and log configurations return errors without panics.
10. WeChat, Email, Webhook, and log overrides round-trip through the admin API without exposing secret plaintext.
11. Enabling or disabling any notifier through the admin takes effect without restarting the notifier list.
12. Alert rules list and route only enabled, configured notification methods.
13. Structured Webhook save rejects invalid templates atomically and never exposes a Rhai editor.
14. Admin-managed log writes cannot escape the working-directory `notifications/` path.
15. Existing expiry, sorting, authentication, install-token, and admin API tests continue to pass.

Final verification includes the full server test suite, locked server/client builds, JavaScript syntax checks, `git diff --check`, and a local integration run with multiple simulated agents. The integration run will stop agents, wait past the configured offline duration, verify retained offline rows, and observe notifier routing without using real production credentials.

## Out of Scope

- SSH, shell, terminal, remote command, or task execution features.
- A durable distributed message queue or guaranteed exactly-once provider delivery.
- Changes to CDN/NPM routing or the Agent report protocol.
- Storing any notification or access credentials in runtime state.
- Exposing Rhai or another executable notification script editor in the admin.
