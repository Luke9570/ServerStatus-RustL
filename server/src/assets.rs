use axum::{
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../web"]
#[prefix = "/"]
pub struct Asset;

pub async fn index_handler() -> impl IntoResponse {
    static_handler(&"/index.html".parse::<Uri>().unwrap())
}

#[allow(unused)]
#[allow(clippy::unused_async)]
pub async fn admin_index_handler() -> impl IntoResponse {
    static_handler(&"/admin.html".parse::<Uri>().unwrap())
}

pub fn static_handler(uri: &Uri) -> impl IntoResponse {
    let path = uri.path().to_string();
    StaticFile(path)
}

pub struct StaticFile<T>(pub T);

fn cache_control_for(path: &str) -> &'static str {
    if matches!(path, "/" | "/index.html" | "/admin.html") || path.ends_with(".html") {
        return "no-store, no-cache, must-revalidate";
    }

    if matches!(
        path,
        "/static/js/expiry.js" | "/static/css/expiry.css" | "/static/js/admin.js" | "/static/css/admin.css"
    ) {
        return "no-cache, must-revalidate";
    }

    "public, max-age=3600"
}

impl<T> IntoResponse for StaticFile<T>
where
    T: Into<String>,
{
    fn into_response(self) -> Response {
        let path = self.0.into();
        match Asset::get(path.as_str()) {
            Some(content) => {
                let mime = mime_guess::from_path(&path).first_or_octet_stream();
                (
                    [
                        (header::CONTENT_TYPE, mime.as_ref()),
                        (header::CACHE_CONTROL, cache_control_for(path.as_str())),
                        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                        (header::REFERRER_POLICY, "same-origin"),
                        (header::X_FRAME_OPTIONS, "DENY"),
                    ],
                    content.data,
                )
                    .into_response()
            }
            None => (StatusCode::NOT_FOUND, "404").into_response(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Asset;

    #[test]
    fn admin_contains_all_notification_modules() {
        let admin_html = Asset::get("/admin.html").expect("embedded admin HTML");
        let admin_html = std::str::from_utf8(admin_html.data.as_ref()).expect("UTF-8 admin HTML");

        for id in ["wechat-save", "email-save", "webhook-save", "log-save"] {
            assert!(
                admin_html.contains(&format!("id=\"{id}\"")),
                "admin HTML should include #{id}"
            );
        }
    }

    #[test]
    fn admin_preserves_alert_notification_selection_mode() {
        let admin_js = Asset::get("/static/js/admin.js").expect("embedded admin JavaScript");
        let admin_js = std::str::from_utf8(admin_js.data.as_ref()).expect("UTF-8 admin JavaScript");

        for contract in [
            "function notificationSelectionForRule(rule)",
            "notificationSelection: notificationSelectionForRule(rule)",
            "function updateAlertRuleNotificationSelection()",
            "const unavailable = [...selection.values].filter((id) => !enabledSet.has(id));",
            "function alertRuleNotificationsForApply()",
            "const notifications = alertRuleNotificationsForApply();",
        ] {
            assert!(
                admin_js.contains(contract),
                "admin JavaScript should preserve alert notification mode: {contract}"
            );
        }
    }

    #[test]
    fn admin_local_saves_keep_global_drafts_isolated() {
        let admin_js = Asset::get("/static/js/admin.js").expect("embedded admin JavaScript");
        let admin_js = std::str::from_utf8(admin_js.data.as_ref()).expect("UTF-8 admin JavaScript");

        let scoped_save = admin_js
            .split("async function saveScopedSettings")
            .nth(1)
            .and_then(|section| section.split("async function readJson").next())
            .expect("scoped settings save helper");
        assert!(
            scoped_save.contains("await getJson(\"/api/admin/settings\")"),
            "local saves must start from a fresh backend settings snapshot"
        );
        assert!(
            scoped_save.contains("settingsPayloadFromSettings(settings.data || {}, overrides)"),
            "local saves must build their complete payload from the backend snapshot"
        );
        assert!(
            scoped_save.contains("preserveState: true") && scoped_save.contains("markClean: false"),
            "local saves must not replace or clean global draft state"
        );

        let snapshot_payload = admin_js
            .split("function settingsPayloadFromSettings")
            .nth(1)
            .and_then(|section| section.split("function settingsPayloadFromState").next())
            .expect("backend snapshot payload helper");
        for field in [
            "hosts: current.hosts || {}",
            "deleted_hosts: current.deleted_hosts || []",
            "access_keys: current.access_keys || {}",
            "deleted_access_keys: current.deleted_access_keys || []",
            "alert_rules: current.alert_rules || []",
        ] {
            assert!(
                snapshot_payload.contains(field),
                "scoped replacement payload must retain backend {field}"
            );
        }

        let notification_save = admin_js
            .split("async function saveNotificationModule")
            .nth(1)
            .and_then(|section| section.split("async function resetNotificationOverride").next())
            .expect("notification save helper");
        let access_save = admin_js
            .split("async function saveAccessSettings")
            .nth(1)
            .and_then(|section| section.split("async function saveExpireNotifySettings").next())
            .expect("access save helper");
        let expire_save = admin_js
            .split("async function saveExpireNotifySettings")
            .nth(1)
            .and_then(|section| section.split("function validAdminUsername").next())
            .expect("expiry save helper");

        for (name, section) in [
            ("notification", notification_save),
            ("access", access_save),
            ("expiry", expire_save),
        ] {
            assert!(
                section.contains("saveScopedSettings("),
                "{name} save must use the isolated save path"
            );
            assert!(
                !section.contains("settingsPayloadFromState("),
                "{name} save must not serialize unrelated UI drafts"
            );
        }

        let notification_reload = admin_js
            .split("async function reloadNotificationState")
            .nth(1)
            .and_then(|section| section.split("async function saveNotificationModule").next())
            .expect("notification reload helper");
        assert!(
            notification_reload.contains("mergeScopedSettings([notificationSettingKey(scope)], settings.data || {})"),
            "notification reload must merge only its saved scope"
        );
    }

    #[test]
    fn admin_settings_replacements_preserve_legacy_data_and_serialize_writes() {
        let admin_js = Asset::get("/static/js/admin.js").expect("embedded admin JavaScript");
        let admin_js = std::str::from_utf8(admin_js.data.as_ref()).expect("UTF-8 admin JavaScript");

        let snapshot_payload = admin_js
            .split("function settingsPayloadFromSettings")
            .nth(1)
            .and_then(|section| section.split("function settingsPayloadFromState").next())
            .expect("backend snapshot payload helper");
        assert!(
            snapshot_payload.contains("notification_groups: current.notification_groups || []"),
            "whole-settings replacements must preserve legacy notification groups with their dependent rules"
        );

        let ensure_settings = admin_js
            .split("function ensureSettings")
            .nth(1)
            .and_then(|section| section.split("function hydrateSettings").next())
            .expect("settings normalization helper");
        assert!(
            !ensure_settings.contains("state.deletedAccessKeys ="),
            "repeated normalization must not overwrite an unsaved access-key deletion draft"
        );
        let hydrate_settings = admin_js
            .split("function hydrateSettings")
            .nth(1)
            .and_then(|section| section.split("function snapshotSettingsState").next())
            .expect("explicit settings hydration helper");
        for contract in [
            "state.deletedHosts = new Set(settings.deleted_hosts || [])",
            "state.deletedAccessKeys = new Set(settings.deleted_access_keys || [])",
        ] {
            assert!(
                hydrate_settings.contains(contract),
                "explicit server loads must hydrate deletion drafts: {contract}"
            );
        }

        let queued_write = admin_js
            .split("function enqueueSettingsWrite")
            .nth(1)
            .and_then(|section| section.split("async function postSettings").next())
            .expect("shared settings write queue");
        assert!(
            queued_write.contains("settingsWriteQueue.then(write, write)"),
            "settings replacements must share one failure-tolerant serialization queue"
        );

        let shared_save = admin_js
            .split("async function saveSettingsPayload")
            .nth(1)
            .and_then(|section| section.split("async function saveScopedSettings").next())
            .expect("shared settings save helper");
        assert!(
            shared_save.contains("await enqueueSettingsWrite(async () => {")
                && shared_save.contains("mergeSavedState?.(saved.data || {})"),
            "scoped response state must merge before the queued turn is released"
        );

        let scoped_save = admin_js
            .split("async function saveScopedSettings")
            .nth(1)
            .and_then(|section| section.split("function mergeScopedSettings").next())
            .expect("scoped settings save helper");
        assert!(
            scoped_save.contains("return saveSettingsPayload(")
                && scoped_save.contains("async () => {")
                && scoped_save.contains("await getJson(\"/api/admin/settings\")"),
            "a scoped save must fetch and build its replacement inside its queued turn"
        );

        let global_save = admin_js
            .split("async function saveDashboard")
            .nth(1)
            .and_then(|section| section.split("async function saveTgbotSettings").next())
            .expect("global dashboard save helper");
        assert!(
            global_save.contains("await enqueueSettingsWrite(async () => {")
                && global_save.contains("postSettings(settingsPayloadFromState())")
                && !global_save.contains("const settingsPayload = settingsPayloadFromState()"),
            "the global save must build its payload only after its queued turn starts"
        );
    }
}
