# Offline Retention and Notification Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keep authenticated servers visible as offline across restarts, make offline alerts reliable, and integrate Telegram, Bark, WeChat, Email, structured Webhook, and local log notifications into the current admin workflow.

**Architecture:** Add a small versioned runtime-state store owned by `StatsMgr`, while retaining the existing public `stats.json` and admin override architecture. Register every notifier once at startup and resolve effective base-plus-admin configuration per event. Preserve legacy Rhai Webhooks from `config.toml`, but expose only validated MiniJinja Webhooks in the admin.

**Tech Stack:** Rust 2021, Axum 0.8, Serde/serde_json, MiniJinja, Reqwest/Rustls, Lettre, vanilla JavaScript, embedded HTML/CSS assets, Cargo tests.

## Global Constraints

- Do not add SSH, remote shell, terminal, command dispatch, remote task execution, or an admin script editor.
- Do not expose agent/group passwords, admin credentials, JWT secrets, Telegram tokens, Bark keys, WeChat secrets, SMTP passwords, Webhook URLs, Webhook passwords, or Webhook header values through admin JSON or logs.
- Keep `config.toml`, legacy Rhai Webhooks, Agent report protocol, reverse-proxy paths, and public `/json/stats.json` compatible.
- `runtime-state.json`, `stats.json`, `admin-overrides.json`, `runtime/`, and `local-test-config.toml` must remain untracked.
- Admin-managed log files must stay below `<working-directory>/notifications/`.
- HTTP retries are limited to three attempts with one- and three-second delays, only for transport errors and HTTP 408, 429, and 5xx.
- Use red-green-refactor for every production change and commit each independently reviewable task.

## File Map

- Create `server/src/runtime_state.rs`: versioned known-host and alert-timing persistence, legacy snapshot import, atomic writes.
- Modify `server/src/main.rs`: register runtime-state module and all notifier implementations unconditionally.
- Modify `server/src/stats.rs`: restore known hosts, retain offline hosts, persist report metadata/alert state, and synchronize deletion.
- Modify `server/src/admin.rs`: notification override models, validation, secret merge/masking, effective configuration.
- Modify `server/src/http.rs`: notification test routing and provider-safe responses.
- Modify `server/src/notifier/{mod,tgbot,bark,wechat,email,webhook,log}.rs`: dynamic configuration, delivery validation, redaction, and panic removal.
- Modify `web/admin.html`: WeChat, Email, structured Webhook, and log settings blocks.
- Modify `web/static/js/admin.js`: form state, per-module saves/tests, secret preservation, dynamic alert choices.
- Modify `web/static/css/admin.css`: responsive notifier layouts and compact receiver/header rows.
- Modify `.gitignore`, `config.toml`, `README.md`, `server/Cargo.toml`, and `Cargo.lock`: runtime file exclusion, compatibility documentation, and version metadata.

---

### Task 1: Versioned Runtime State Store

**Files:**
- Create: `server/src/runtime_state.rs`
- Modify: `server/src/main.rs`
- Modify: `.gitignore`

**Interfaces:**
- Produces: `RuntimeStateStore::load(path)`, `snapshot()`, `upsert_host(KnownHost)`, `purge_hosts(&HashSet<String>)`, `replace_alerts(HashMap<String, AlertState>)`, `import_legacy_stats(path, deleted_hosts)`, and `save()`.
- Produces: `KnownHost::from_stat(&HostStat)` and `KnownHost::into_offline_stat()`.
- Consumes: `crate::payload::HostStat` and `crate::expiry::ExpireInfo`.

- [ ] **Step 1: Write failing serialization, secret-exclusion, corrupt-file, and atomic-round-trip tests**

```rust
#[test]
fn runtime_state_round_trip_restores_host_offline_without_secrets() {
    let dir = std::env::temp_dir().join(format!("ssr-runtime-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("runtime-state.json");
    let store = RuntimeStateStore::load(path.clone());
    store.upsert_host(KnownHost::from_stat(&HostStat {
        name: "srv-1".into(),
        alias: "PVE".into(),
        gid: "default".into(),
        online4: true,
        latest_ts: 123,
        ..Default::default()
    }));
    store.save().unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("password"));
    assert!(!text.contains("token"));
    let restored = RuntimeStateStore::load(path).snapshot();
    let stat = restored.hosts["srv-1"].clone().into_offline_stat();
    assert!(!stat.online4 && !stat.online6);
    assert_eq!(stat.latest_ts, 123);
}

#[test]
fn corrupt_runtime_state_is_ignored() {
    let path = std::env::temp_dir().join(format!("ssr-runtime-{}.json", uuid::Uuid::new_v4()));
    std::fs::write(&path, "{not-json").unwrap();
    assert!(RuntimeStateStore::load(path).snapshot().hosts.is_empty());
}

#[test]
fn legacy_stats_import_recovers_public_fields_and_skips_deleted_ids() {
    let path = std::env::temp_dir().join(format!("ssr-stats-{}.json", uuid::Uuid::new_v4()));
    std::fs::write(
        &path,
        r#"{"updated":200,"servers":[{"name":"keep","alias":"PVE","type":"kvm","location":"sg","gid":"default","labels":"os=debian","latest_ts":123},{"name":"gone"}]}"#,
    ).unwrap();
    let imported = import_legacy_stats(&path, &HashSet::from(["gone".into()]));
    assert_eq!(imported.len(), 1);
    assert_eq!(imported[0].name, "keep");
    assert_eq!(imported[0].latest_ts, 123);
}
```

- [ ] **Step 2: Run tests and verify RED**

Run: `cargo test -p stat_server runtime_state --locked`

Expected: FAIL because `runtime_state` and `RuntimeStateStore` do not exist.

- [ ] **Step 3: Implement the focused runtime-state types and atomic save**

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimeState {
    pub version: u32,
    pub hosts: HashMap<String, KnownHost>,
    pub alerts: HashMap<String, AlertState>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AlertState {
    pub since: u64,
    pub last_enqueued_at: u64,
}

pub struct RuntimeStateStore {
    path: PathBuf,
    inner: Mutex<RuntimeState>,
}

impl RuntimeStateStore {
    pub fn load(path: PathBuf) -> Self {
        let state = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<RuntimeState>(&text).ok())
            .filter(|state| state.version == RUNTIME_STATE_VERSION)
            .unwrap_or_else(|| RuntimeState {
                version: RUNTIME_STATE_VERSION,
                ..RuntimeState::default()
            });
        Self { path, inner: Mutex::new(state) }
    }
    pub fn snapshot(&self) -> RuntimeState { self.inner.lock().unwrap().clone() }
    pub fn save(&self) -> Result<()> {
        let payload = serde_json::to_vec_pretty(&*self.inner.lock().unwrap())?;
        let temporary = self.path.with_extension("json.tmp");
        fs::write(&temporary, payload)?;
        File::open(&temporary)?.sync_all()?;
        fs::rename(temporary, &self.path)?;
        Ok(())
    }
}
```

`KnownHost` contains only public metadata and alert inputs listed in the design. Add `runtime-state.json` to `.gitignore` even though Docker stores it under ignored `runtime/`.

- [ ] **Step 4: Run focused tests and verify GREEN**

Run: `cargo test -p stat_server runtime_state --locked`

Expected: all `runtime_state` tests PASS and corrupt input logs a warning without panic.

- [ ] **Step 5: Commit**

```bash
git add .gitignore server/src/main.rs server/src/runtime_state.rs
git commit -m "Persist known server runtime state"
```

### Task 2: Retain and Restore Offline Servers

**Files:**
- Modify: `server/src/stats.rs`
- Test: inline tests in `server/src/stats.rs`

**Interfaces:**
- Consumes: `RuntimeStateStore`, `KnownHost` from Task 1.
- Produces: `mark_offline_if_stale(stat, now, threshold) -> bool` and `StatsMgr` startup restoration.

- [ ] **Step 1: Add failing lifecycle tests**

```rust
#[test]
fn stale_dynamic_host_is_marked_offline_but_remains_publishable() {
    let mut stat = HostStat {
        name: "pve-child".into(), gid: "default".into(),
        online4: true, online6: true, latest_ts: 100, ..Default::default()
    };
    assert!(mark_offline_if_stale(&mut stat, 131, 30));
    assert!(!stat.online4 && !stat.online6);
    assert!(should_publish_stat(&stat, &HashSet::new()));
}

#[test]
fn deleted_host_is_not_published_until_authenticated_report_clears_marker() {
    let stat = HostStat { name: "deleted".into(), ..Default::default() };
    assert!(!should_publish_stat(&stat, &HashSet::from(["deleted".into()])));
    assert!(should_process_reported_stat(&stat, &HashSet::from(["deleted".into()])));
}
```

- [ ] **Step 2: Run focused tests and verify RED**

Run: `cargo test -p stat_server stale_dynamic_host deleted_host_is_not_published --locked`

Expected: FAIL because stale group hosts are still garbage-collected and deleted IDs are still published.

- [ ] **Step 3: Implement restoration and retention**

Remove the `hosts_map`/`stat_map` `group_gc` retention block. Load `KnownHost` rows into `stat_map` before timer publication, force both online flags false, filter `admin::deleted_hosts()`, apply current host overrides, sort, and publish the first response immediately. On every valid report, update `RuntimeStateStore`; save immediately for a new ID and on the existing 60-second cadence thereafter.

```rust
fn should_publish_stat(stat: &HostStat, deleted: &HashSet<String>) -> bool {
    !stat.name.trim().is_empty() && !deleted.contains(&stat.name)
}

fn mark_offline_if_stale(stat: &mut HostStat, now: u64, threshold: u64) -> bool {
    if stat.latest_ts.saturating_add(threshold) < now {
        stat.online4 = false;
        stat.online6 = false;
        return true;
    }
    false
}
```

- [ ] **Step 4: Verify lifecycle tests and existing sorting tests**

Run: `cargo test -p stat_server stats::tests --locked`

Expected: PASS, with offline nodes below online nodes and no group-age removal.

- [ ] **Step 5: Commit**

```bash
git add server/src/stats.rs
git commit -m "Retain offline servers until admin deletion"
```

### Task 3: Persist Alert Timing and Synchronize Deletion

**Files:**
- Modify: `server/src/runtime_state.rs`
- Modify: `server/src/stats.rs`
- Modify: `server/src/http.rs`
- Test: inline tests in those modules

**Interfaces:**
- Consumes: `RuntimeStateStore::alerts` from Task 1.
- Produces: `prune_alert_states(active_rules, active_hosts)` and deletion that purges runtime/public/alert state.

- [ ] **Step 1: Add failing tests for unrestricted offline alerts, repeat persistence, and purge**

```rust
#[test]
fn unrestricted_offline_rule_routes_only_selected_bark() {
    let stat = HostStat { name: "pve".into(), latest_ts: 100, notify: true, online4: false, online6: false, ..Default::default() };
    let rule = AlertRuleOverride {
        id: "offline-all".into(), metric: "offline".into(), duration: 30,
        repeat_interval: 3600, notifications: vec!["bark".into()], ..Default::default()
    };
    let events = collect_alert_events(&stat, 131, &[rule], &[], &mut HashMap::new());
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].2, vec!["bark"]);
}

#[test]
fn purge_removes_host_and_all_prefixed_alert_keys() {
    let path = std::env::temp_dir().join(format!("ssr-runtime-{}.json", uuid::Uuid::new_v4()));
    let state = RuntimeStateStore::load(path);
    state.upsert_host(KnownHost { name: "pve".into(), ..Default::default() });
    state.replace_alerts(HashMap::from([(
        "pve:offline-all".into(),
        AlertState { since: 100, last_enqueued_at: 131 },
    )]));
    state.purge_hosts(&HashSet::from(["pve".into()]));
    assert!(state.snapshot().hosts.is_empty());
    assert!(state.snapshot().alerts.is_empty());
}
```

- [ ] **Step 2: Run focused tests and verify RED**

Run: `cargo test -p stat_server offline_rule purge_removes_host --locked`

Expected: persistence/purge test FAIL because alert state is timer-local.

- [ ] **Step 3: Move alert state into the store and prune inactive keys**

Use the stable key format `server_id:rule_id`. After each evaluation cycle, retain only keys whose server exists and whose rule ID remains enabled. Update `last_enqueued_at` only after `notifier_tx.send(...)` succeeds. Persist state on changed timestamps and deletion.

`StatsMgr::purge_hosts` must remove the same IDs from `stat_map`, `hosts_map`, `stats_data`, runtime hosts, and runtime alerts before rewriting both public JSON and disk state.

- [ ] **Step 4: Run alert and deletion tests**

Run: `cargo test -p stat_server alert purge deleted --locked`

Expected: PASS, including the existing same-ID re-report behavior.

- [ ] **Step 5: Commit**

```bash
git add server/src/runtime_state.rs server/src/stats.rs server/src/http.rs
git commit -m "Persist alert timing and deletion state"
```

### Task 4: Admin Models, Validation, and Secret Masking

**Files:**
- Modify: `server/src/admin.rs`
- Modify: `server/src/config.rs`
- Test: inline tests in `server/src/admin.rs`

**Interfaces:**
- Produces: `WechatOverride`, `EmailOverride`, `StructuredWebhookOverride`, `StructuredWebhookReceiver`, `WebhookHeaderOverride`, and `LogOverride`.
- Produces: `effective_wechat_config`, `effective_email_config`, `effective_webhook_override`, `effective_log_config`, and `configured_notification_methods`.

- [ ] **Step 1: Add failing round-trip and redaction tests**

```rust
#[test]
fn public_snapshot_masks_all_new_notification_secrets() {
    let data = AdminData {
        wechat: Some(WechatOverride { corp_secret: "wechat-secret".into(), ..Default::default() }),
        email: Some(EmailOverride { password: "smtp-secret".into(), ..Default::default() }),
        webhook: Some(StructuredWebhookOverride {
            receivers: vec![StructuredWebhookReceiver {
                url: "https://hooks.example/secret".into(),
                password: "basic-secret".into(),
                headers: vec![WebhookHeaderOverride { name: "Authorization".into(), value: "Bearer secret".into(), ..Default::default() }],
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    let public = public_admin_data(&data);
    let json = serde_json::to_string(&public).unwrap();
    for secret in ["wechat-secret", "smtp-secret", "hooks.example/secret", "basic-secret", "Bearer secret"] {
        assert!(!json.contains(secret));
    }
}
```

- [ ] **Step 2: Run admin tests and verify RED**

Run: `cargo test -p stat_server admin::tests::public_snapshot_masks_all_new_notification_secrets --locked`

Expected: FAIL because the new override types and fields do not exist.

- [ ] **Step 3: Implement normalized override structs and merge rules**

Follow the existing Telegram/Bark `clear_*` plus `*_configured` pattern. For Webhook headers, preserve values by stable receiver ID plus case-insensitive header name. Validate IDs, URL scheme (`http`/`https` only), timeout range `1..=60`, unique header names, MiniJinja compilation, Email addresses, and safe log template.

```rust
pub fn configured_notification_methods(cfg: &Config) -> Vec<String> {
    let mut methods = Vec::new();
    if effective_tgbot_config(&cfg.tgbot).is_ready() { methods.push("tg".into()); }
    if effective_bark_config(&cfg.bark).is_ready() { methods.push("bark".into()); }
    if effective_wechat_config(&cfg.wechat).is_ready() { methods.push("wechat".into()); }
    if effective_email_config(&cfg.email).is_ready() { methods.push("email".into()); }
    if effective_webhook_override(&cfg.webhook).is_ready() { methods.push("webhook".into()); }
    if effective_log_config(&cfg.log).is_ready() { methods.push("log".into()); }
    methods
}
```

- [ ] **Step 4: Run all admin/config tests**

Run: `cargo test -p stat_server admin::tests config::tests --locked`

Expected: PASS with no secret plaintext in serialized admin responses.

- [ ] **Step 5: Commit**

```bash
git add server/src/admin.rs server/src/config.rs
git commit -m "Add secure notification admin models"
```

### Task 5: Dynamic Notifier Registration and Failure Isolation

**Files:**
- Modify: `server/src/main.rs`
- Modify: `server/src/notifier/mod.rs`
- Modify: `server/src/notifier/email.rs`
- Modify: `server/src/notifier/log.rs`
- Test: inline notifier tests

**Interfaces:**
- Consumes: effective-config functions from Task 4.
- Produces: every notifier registered once; disabled methods perform no I/O; notifier errors are logged and isolated.

- [ ] **Step 1: Add failing readiness and no-panic tests**

```rust
#[test]
fn invalid_email_addresses_return_error_without_panic() {
    let config = Config { enabled: true, username: "not-an-email".into(), to: "bad".into(), ..Default::default() };
    assert!(build_message(&config, "test").is_err());
}

#[test]
fn admin_log_path_is_fixed_below_notifications_directory() {
    let path = admin_log_path(Path::new("/data"), "2026-07-14");
    assert_eq!(path, Path::new("/data/notifications/ssr.log.2026-07-14"));
}
```

- [ ] **Step 2: Run notifier tests and verify RED**

Run: `cargo test -p stat_server notifier --locked`

Expected: FAIL because message/path helpers and dynamic effective configs do not exist.

- [ ] **Step 3: Register all notifiers unconditionally and remove panic paths**

Push Telegram, Bark, WeChat, Email, Log, and Webhook into the notifier vector regardless of base enabled state. Each notifier checks effective config inside `notify` and `notify_test`. Replace Email and log `unwrap`/`panic` with `Result` and structured errors. In the dispatcher:

```rust
if let Err(err) = notifier.notify(&msg.event, &msg.stat) {
    error!("notification failed: kind={}, event={:?}, server={}, error={err:#}",
        notifier.kind(), msg.event, msg.stat.name);
}
```

- [ ] **Step 4: Run notifier and main tests**

Run: `cargo test -p stat_server notifier --locked`

Expected: PASS; invalid Email/log configuration cannot panic.

- [ ] **Step 5: Commit**

```bash
git add server/src/main.rs server/src/notifier/mod.rs server/src/notifier/email.rs server/src/notifier/log.rs
git commit -m "Make notification modules runtime configurable"
```

### Task 6: HTTP Provider Validation, Retry, and Structured Webhook

**Files:**
- Modify: `server/src/notifier/mod.rs`
- Modify: `server/src/notifier/tgbot.rs`
- Modify: `server/src/notifier/bark.rs`
- Modify: `server/src/notifier/wechat.rs`
- Modify: `server/src/notifier/webhook.rs`
- Test: inline async tests using local Axum listeners

**Interfaces:**
- Produces: `is_retryable_status(StatusCode)`, provider response parsers, redaction helpers, and structured MiniJinja Webhook delivery.
- Consumes: structured Webhook models from Task 4.

- [ ] **Step 1: Add failing provider response and retry tests**

```rust
#[test]
fn retry_policy_is_bounded_to_transient_statuses() {
    assert!(is_retryable_status(StatusCode::REQUEST_TIMEOUT));
    assert!(is_retryable_status(StatusCode::TOO_MANY_REQUESTS));
    assert!(is_retryable_status(StatusCode::BAD_GATEWAY));
    assert!(!is_retryable_status(StatusCode::BAD_REQUEST));
    assert!(!is_retryable_status(StatusCode::UNAUTHORIZED));
}

#[test]
fn webhook_error_text_never_contains_credentials() {
    let detail = sanitize_webhook_error(
        "https://hooks.example/secret Authorization: Bearer abc",
        &["https://hooks.example/secret", "Bearer abc"],
    );
    assert_eq!(detail, "[redacted] Authorization: [redacted]");
}
```

- [ ] **Step 2: Run focused tests and verify RED**

Run: `cargo test -p stat_server retry_policy webhook_error wechat_response --locked`

Expected: FAIL because shared retry/provider helpers are absent.

- [ ] **Step 3: Implement three-attempt delivery loops and provider validation**

Telegram requires HTTP success plus JSON `ok=true`; Bark accepts provider code `0` or `200`; WeChat requires HTTP success plus `errcode=0` for token and send calls. Webhook requires HTTP success and logs status only on failure. Delay one second before attempt two and three seconds before attempt three. Redact provider secrets before any error reaches logs or HTTP responses.

For admin Webhooks, render MiniJinja with `event`, `host`, `config`, `ip_info`, and `sys_info`; never invoke Rhai. For base `config.toml` receivers, retain the existing Rhai evaluation but compile errors disable only that receiver.

- [ ] **Step 4: Run all notifier tests**

Run: `cargo test -p stat_server notifier --locked`

Expected: PASS with exactly three requests for transient failure fixtures and one request for permanent 4xx fixtures.

- [ ] **Step 5: Commit**

```bash
git add server/src/notifier/tgbot.rs server/src/notifier/bark.rs server/src/notifier/wechat.rs server/src/notifier/webhook.rs
git commit -m "Harden notification provider delivery"
```

### Task 7: Admin Save and Test APIs for All Methods

**Files:**
- Modify: `server/src/http.rs`
- Modify: `server/src/main.rs`
- Test: inline tests in `server/src/http.rs` and `server/src/admin.rs`

**Interfaces:**
- Extends: `NotifyTestPayload` with `wechat`, `email`, `webhook`, and `log`.
- Extends: `POST /api/admin/notify-test/{kind}` for all six methods.

- [ ] **Step 1: Add failing API dispatch and 401 coverage**

```rust
#[tokio::test]
async fn notification_test_routes_require_admin_auth() {
    for kind in ["tgbot", "bark", "wechat", "email", "webhook", "log"] {
        let response = test_router().oneshot(
            Request::post(format!("/api/admin/notify-test/{kind}"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        ).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
```

- [ ] **Step 2: Run HTTP tests and verify RED**

Run: `cargo test -p stat_server notification_test_routes --locked`

Expected: FAIL for unsupported new kinds or missing test fixtures.

- [ ] **Step 3: Route tests through notifier-owned validation helpers**

Avoid duplicating provider request construction in `http.rs`. Each notifier exposes a test function returning a redacted success message or `anyhow::Error`; HTTP maps validation to 400, provider/transport failure to 502, and unsupported kind to 404. `save_admin_settings` maps validation failures to 400 and leaves previous settings unchanged.

- [ ] **Step 4: Run HTTP/admin/JWT tests**

Run: `cargo test -p stat_server http admin jwt --locked`

Expected: PASS; every protected endpoint remains 401 without a valid admin token.

- [ ] **Step 5: Commit**

```bash
git add server/src/http.rs server/src/main.rs server/src/admin.rs
git commit -m "Expose secure tests for all notification methods"
```

### Task 8: Admin Notification UI and Dynamic Alert Choices

**Files:**
- Modify: `server/src/assets.rs`
- Modify: `web/admin.html`
- Modify: `web/static/js/admin.js`
- Modify: `web/static/css/admin.css`

**Interfaces:**
- Consumes: masked admin settings and test endpoints from Tasks 4 and 7.
- Produces: local scopes `wechat`, `email`, `webhook`, and `log`; dynamic enabled-method alert checkboxes.

- [ ] **Step 1: Add a failing static DOM contract test**

Create a small Rust asset test or shell-verifiable contract asserting the embedded admin contains these IDs:

```rust
#[test]
fn admin_contains_all_notification_modules() {
    let html = include_str!("../../web/admin.html");
    for id in ["wechat-save", "email-save", "webhook-save", "log-save"] {
        assert!(html.contains(&format!("id=\"{id}\"")));
    }
}
```

- [ ] **Step 2: Run the contract test and JavaScript syntax check; verify RED**

Run: `cargo test -p stat_server admin_contains_all_notification_modules --locked`

Expected: FAIL because the new modules are absent.

Run: `node --check web/static/js/admin.js`

Expected: current file passes before edits, establishing the syntax baseline.

- [ ] **Step 3: Add compact modules following existing Telegram/Bark patterns**

Add module headers with Test and Save controls, dirty-state baselines, masked secret inputs with reveal/clear icons, and inline result outputs. Webhook receiver rows use stable IDs and one-line icon actions for add/delete; header rows expose names and masked values without nested cards. Log exposes only enabled and template fields.

Each inherited module also has a reset-to-config-file command. It deletes only that module's admin override after confirmation, reloads the effective masked state from the backend, and never clears unrelated notification settings.

Generate alert notification checkboxes from:

```javascript
function enabledNotificationMethods() {
  return [
    ["tg", "Telegram", notificationReady("tgbot")],
    ["bark", "Bark", notificationReady("bark")],
    ["wechat", "企业微信", notificationReady("wechat")],
    ["email", "Email", notificationReady("email")],
    ["webhook", "Webhook", notificationReady("webhook")],
    ["log", "本地日志", notificationReady("log")],
  ].filter(([, , ready]) => ready);
}
```

If this array is empty, render `请先配置并启用通知方式` and do not save a fabricated method.

- [ ] **Step 4: Verify syntax, DOM contract, and responsive CSS**

Run: `node --check web/static/js/admin.js`

Run: `cargo test -p stat_server admin_contains_all_notification_modules --locked`

Run: `git diff --check`

Expected: all PASS; no overflowing receiver/header actions at 390px and 1440px viewports.

- [ ] **Step 5: Commit**

```bash
git add web/admin.html web/static/js/admin.js web/static/css/admin.css
git commit -m "Add inherited notifications to admin UI"
```

### Task 9: Documentation, Compatibility, and Version Metadata

**Files:**
- Modify: `config.toml`
- Modify: `README.md`
- Modify: `server/Cargo.toml`
- Modify: `Cargo.lock`

**Interfaces:**
- Documents: offline retention, runtime-state file, six admin notification methods, safe Webhook behavior, and Docker persistence.

- [ ] **Step 1: Update docs and version to 1.8.5**

Document that `group_gc` is retained for parsing but no longer removes authenticated servers, `runtime/` must remain mounted, admin Webhooks are structured, and legacy Rhai is config-file-only. Keep secrets as placeholders. Set `server/Cargo.toml` version to `1.8.5`; let Cargo update the lockfile package entry.

- [ ] **Step 2: Validate sample configuration**

Run: `cargo run -p stat_server --locked -- -c config.toml -t`

Expected: exit 0 and configuration valid; no real credentials printed.

- [ ] **Step 3: Commit**

```bash
git add config.toml README.md server/Cargo.toml Cargo.lock
git commit -m "Document persistent alerts and notifications"
```

### Task 10: Full Integration, Security Review, and Push

**Files:**
- Modify only files required by failures discovered in this task.
- Do not stage: `local-test-config.toml`, `runtime/`, `runtime-state.json`, `stats.json`, `admin-overrides.json`.

**Interfaces:**
- Verifies the complete design against real embedded assets and simulated agents.

- [ ] **Step 1: Run complete automated verification**

Run:

```bash
cargo check -p stat_server --locked
cargo test -p stat_server --locked
cargo build -p stat_server -p stat_client --locked
node --check web/static/js/admin.js
git diff --check
```

Expected: all commands exit 0 and the server test summary reports zero failures.

- [ ] **Step 2: Run local multi-agent offline integration**

Start the server with `local-test-config.toml`, run at least three clients in the `default` group, stop the clients, wait past `offline_threshold` and the test rule duration, then verify:

```bash
curl -fsS http://127.0.0.1:18080/json/stats.json
```

Expected: all three IDs remain in `servers`, each has `online4=false` and `online6=false`, and the selected local log notifier records one offline Health event per server. Restart the server and verify the rows remain offline without duplicate alerts inside `repeat_interval`.

- [ ] **Step 3: Verify admin behavior in the in-app browser**

At desktop and mobile widths verify: no login flash, all six notifier modules render, light/dark contrast remains readable, secret fields show configured masks, Save is disabled after reverting changes, tests show inline feedback, Webhook rows do not overflow, and alert rules show only enabled methods.

- [ ] **Step 4: Perform final secret and unsafe-capability scan**

Run:

```bash
rg -n 'device_key\s*=\s*\"[A-Za-z0-9]{12,}\"|bot_token\s*=\s*\"[^<][^\"]+\"|jwt_secret\s*=\s*\"[^\"]+\"|admin_pass\s*=\s*\"[^\"]+\"' --glob '!local-test-config.toml' --glob '!runtime/**' .
rg -n "ssh|remote shell|Command::new|std::process::Command" server/src web README.md
git status --short
```

Expected: no real credential matches; no newly introduced command-execution capability; only intended tracked changes plus untracked `local-test-config.toml`.

- [ ] **Step 5: Review commits, push `main`, and verify remote**

```bash
git log --oneline origin/main..main
git push origin main
git ls-remote origin refs/heads/main
```

Expected: push succeeds and the remote main SHA equals local `git rev-parse HEAD`.

- [ ] **Step 6: Provide VPS update commands**

```bash
cd /home/docker_data/ServerStatus-RustL
cp config.toml config.toml.local.bak
git fetch origin
git stash push -m "serverstatus-local-config-before-update" -- config.toml
git switch -C main origin/main
cp config.toml.local.bak config.toml
docker compose pull
docker compose up -d --force-recreate
docker compose logs --tail=80 stat_server
```

Explain that browser/CDN cache normally does not need manual clearing because embedded assets ship in the new image; if a CDN caches HTML/static assets aggressively, purge only the panel cache after confirming the container SHA/image changed.
