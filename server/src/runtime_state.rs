use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::expiry::ExpireInfo;
use crate::payload::HostStat;

const RUNTIME_STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownHost {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub alias: String,
    #[serde(default)]
    pub gid: String,
    #[serde(default)]
    pub location: String,
    #[serde(rename = "type", default)]
    pub host_type: String,
    #[serde(default)]
    pub labels: String,
    #[serde(default)]
    pub weight: u64,
    #[serde(default)]
    pub expire: ExpireInfo,
    #[serde(default = "default_as_true")]
    pub notify: bool,
    #[serde(default = "default_as_true")]
    pub expire_notify: bool,
    #[serde(default)]
    pub latest_ts: u64,
}

impl Default for KnownHost {
    fn default() -> Self {
        Self {
            name: String::new(),
            alias: String::new(),
            gid: String::new(),
            location: String::new(),
            host_type: String::new(),
            labels: String::new(),
            weight: 0,
            expire: ExpireInfo::default(),
            notify: true,
            expire_notify: true,
            latest_ts: 0,
        }
    }
}

impl KnownHost {
    pub fn from_stat(stat: &HostStat) -> Self {
        Self {
            name: stat.name.clone(),
            alias: stat.alias.clone(),
            gid: stat.gid.clone(),
            location: stat.location.clone(),
            host_type: stat.host_type.clone(),
            labels: stat.labels.clone(),
            weight: stat.weight,
            expire: stat.expire.clone(),
            notify: stat.notify,
            expire_notify: stat.expire_notify,
            latest_ts: stat.latest_ts,
        }
    }

    pub fn into_offline_stat(self) -> HostStat {
        HostStat {
            name: self.name,
            alias: self.alias,
            gid: self.gid,
            location: self.location,
            host_type: self.host_type,
            labels: self.labels,
            weight: self.weight,
            expire: self.expire,
            notify: self.notify,
            expire_notify: self.expire_notify,
            latest_ts: self.latest_ts,
            online4: false,
            online6: false,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertState {
    #[serde(default)]
    pub since: u64,
    #[serde(default)]
    pub last_enqueued_at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimeState {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub hosts: HashMap<String, KnownHost>,
    #[serde(default)]
    pub alerts: HashMap<String, AlertState>,
}

struct VersionedRuntimeState {
    generation: u64,
    state: RuntimeState,
}

pub struct RuntimeStateStore {
    path: PathBuf,
    inner: Mutex<VersionedRuntimeState>,
    publication_lock: Mutex<()>,
}

impl RuntimeStateStore {
    pub fn load(path: PathBuf) -> Self {
        let state = match fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<RuntimeState>(&text) {
                Ok(state) if state.version == RUNTIME_STATE_VERSION => state,
                Ok(state) => {
                    warn!("ignore unsupported runtime state version {}", state.version);
                    empty_runtime_state()
                }
                Err(err) => {
                    warn!("ignore corrupt runtime state: {err}");
                    empty_runtime_state()
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => empty_runtime_state(),
            Err(err) => {
                warn!("could not read runtime state: {err}");
                empty_runtime_state()
            }
        };

        Self {
            path,
            inner: Mutex::new(VersionedRuntimeState { generation: 0, state }),
            publication_lock: Mutex::new(()),
        }
    }

    pub fn snapshot(&self) -> RuntimeState {
        self.inner.lock().unwrap().state.clone()
    }

    pub fn upsert_host(&self, host: KnownHost) {
        if host.name.trim().is_empty() {
            return;
        }
        let mut state = self.inner.lock().unwrap();
        state.state.hosts.insert(host.name.clone(), host);
        state.generation = state.generation.wrapping_add(1);
    }

    pub fn purge_hosts(&self, hosts: &HashSet<String>) -> bool {
        if hosts.is_empty() {
            return false;
        }

        let mut state = self.inner.lock().unwrap();
        let before_hosts = state.state.hosts.len();
        let before_alerts = state.state.alerts.len();
        state.state.hosts.retain(|name, _| !hosts.contains(name));
        state
            .state
            .alerts
            .retain(|key, _| key.split_once(':').is_none_or(|(host, _)| !hosts.contains(host)));
        let changed = state.state.hosts.len() != before_hosts || state.state.alerts.len() != before_alerts;
        if changed {
            state.generation = state.generation.wrapping_add(1);
        }
        changed
    }

    pub fn replace_alerts(&self, alerts: HashMap<String, AlertState>) {
        let mut state = self.inner.lock().unwrap();
        if state.state.alerts != alerts {
            state.state.alerts = alerts;
            state.generation = state.generation.wrapping_add(1);
        }
    }

    pub fn update_alerts<T>(&self, update: impl FnOnce(&mut HashMap<String, AlertState>) -> T) -> (T, bool) {
        let mut state = self.inner.lock().unwrap();
        let previous = state.state.alerts.clone();
        let result = update(&mut state.state.alerts);
        let changed = state.state.alerts != previous;
        if changed {
            state.generation = state.generation.wrapping_add(1);
        }
        (result, changed)
    }

    pub fn mark_alert_enqueued(&self, key: &str, now: u64) -> bool {
        let mut state = self.inner.lock().unwrap();
        let Some(alert) = state.state.alerts.get_mut(key) else {
            return false;
        };
        if alert.last_enqueued_at == now {
            return false;
        }
        alert.last_enqueued_at = now;
        state.generation = state.generation.wrapping_add(1);
        true
    }

    pub fn prune_alert_states(&self, active_rules: &HashSet<String>, active_hosts: &HashSet<String>) -> bool {
        let mut state = self.inner.lock().unwrap();
        let before = state.state.alerts.len();
        state.state.alerts.retain(|key, _| {
            key.split_once(':')
                .is_some_and(|(host, rule)| active_hosts.contains(host) && active_rules.contains(rule))
        });
        let changed = state.state.alerts.len() != before;
        if changed {
            state.generation = state.generation.wrapping_add(1);
        }
        changed
    }

    pub fn save(&self) -> Result<()> {
        self.save_with_before_publish(|| {})
    }

    fn save_with_before_publish(&self, before_publish: impl FnOnce()) -> Result<()> {
        let (payload, generation) = {
            let state = self.inner.lock().unwrap();
            (serde_json::to_vec_pretty(&state.state)?, state.generation)
        };
        if let Some(parent) = self.path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }

        let temporary = self.path.with_extension(format!("json.{}.tmp", uuid::Uuid::new_v4()));
        fs::write(&temporary, payload)?;
        File::open(&temporary)?.sync_all()?;
        before_publish();

        let _publication = self.publication_lock.lock().unwrap();
        let state = self.inner.lock().unwrap();
        if state.generation != generation {
            drop(state);
            fs::remove_file(&temporary)?;
            return Ok(());
        }
        fs::rename(temporary, &self.path)?;
        Ok(())
    }
}

pub fn import_legacy_stats(path: &Path, deleted_hosts: &HashSet<String>) -> Vec<KnownHost> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            warn!("could not read legacy stats: {err}");
            return Vec::new();
        }
    };
    let stats = match serde_json::from_str::<LegacyStats>(&text) {
        Ok(stats) => stats,
        Err(err) => {
            warn!("ignore corrupt legacy stats: {err}");
            return Vec::new();
        }
    };

    stats
        .servers
        .into_iter()
        .filter_map(|server| match serde_json::from_value::<KnownHost>(server) {
            Ok(host) if !host.name.trim().is_empty() && !deleted_hosts.contains(&host.name) => Some(host),
            Ok(_) => None,
            Err(err) => {
                warn!("ignore malformed legacy stats row: {err}");
                None
            }
        })
        .collect()
}

fn default_as_true() -> bool {
    true
}

fn empty_runtime_state() -> RuntimeState {
    RuntimeState {
        version: RUNTIME_STATE_VERSION,
        ..RuntimeState::default()
    }
}

#[derive(Deserialize)]
struct LegacyStats {
    #[serde(default)]
    servers: Vec<Value>,
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::{import_legacy_stats, AlertState, KnownHost, RuntimeStateStore};
    use crate::payload::HostStat;

    fn temporary_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("ssr-{name}-{}", uuid::Uuid::new_v4()))
    }

    fn remove_file_if_present(path: &Path) {
        if path.exists() {
            fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn runtime_state_round_trip_restores_host_offline_without_secrets() {
        let dir = temporary_path("runtime");
        fs::create_dir_all(&dir).unwrap();
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

        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("password"));
        assert!(!text.contains("token"));
        assert!(!path.with_extension("json.tmp").exists());

        let restored = RuntimeStateStore::load(path.clone()).snapshot();
        let stat = restored.hosts["srv-1"].clone().into_offline_stat();
        assert!(!stat.online4 && !stat.online6);
        assert_eq!(stat.latest_ts, 123);

        remove_file_if_present(&path);
        fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn corrupt_or_unsupported_runtime_state_is_ignored() {
        let corrupt = temporary_path("runtime-corrupt.json");
        fs::write(&corrupt, "{not-json").unwrap();
        assert!(RuntimeStateStore::load(corrupt.clone()).snapshot().hosts.is_empty());

        let unsupported = temporary_path("runtime-unsupported.json");
        fs::write(&unsupported, r#"{"version":999,"hosts":{"srv-1":{}}}"#).unwrap();
        assert!(RuntimeStateStore::load(unsupported.clone()).snapshot().hosts.is_empty());

        remove_file_if_present(&corrupt);
        remove_file_if_present(&unsupported);
    }

    #[test]
    fn legacy_stats_import_recovers_public_fields_and_skips_deleted_ids() {
        let path = temporary_path("stats.json");
        fs::write(
            &path,
            r#"{"updated":200,"servers":[{"name":"keep","alias":"PVE","type":"kvm","location":"sg","gid":"default","labels":"os=debian","latest_ts":123},{"name":"gone"},{"alias":"missing-id"}]}"#,
        )
        .unwrap();

        let imported = import_legacy_stats(&path, &HashSet::from(["gone".into()]));
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].name, "keep");
        assert_eq!(imported[0].latest_ts, 123);
        assert_eq!(imported[0].labels, "os=debian");

        remove_file_if_present(&path);
    }

    #[test]
    fn runtime_state_updates_and_purges_hosts_with_their_alerts() {
        let path = temporary_path("runtime-state.json");
        let store = RuntimeStateStore::load(path.clone());
        store.upsert_host(KnownHost {
            name: "pve".into(),
            ..Default::default()
        });
        store.replace_alerts(HashMap::from([(
            "pve:offline".into(),
            AlertState {
                since: 100,
                last_enqueued_at: 131,
            },
        )]));

        store.purge_hosts(&HashSet::from(["pve".into()]));
        let state = store.snapshot();
        assert!(state.hosts.is_empty());
        assert!(state.alerts.is_empty());

        remove_file_if_present(&path);
    }

    #[test]
    fn purge_prevents_stale_runtime_save_from_overwriting_disk() {
        let path = temporary_path("runtime-save-race.json");
        let store = std::sync::Arc::new(RuntimeStateStore::load(path.clone()));
        store.upsert_host(KnownHost {
            name: "pve".into(),
            ..Default::default()
        });
        store.replace_alerts(HashMap::from([(
            "pve:offline".into(),
            AlertState {
                since: 100,
                last_enqueued_at: 131,
            },
        )]));

        let (snapshot_ready_tx, snapshot_ready_rx) = std::sync::mpsc::channel();
        let (publish_tx, publish_rx) = std::sync::mpsc::channel();
        let stale_saver = {
            let store = std::sync::Arc::clone(&store);
            std::thread::spawn(move || {
                store
                    .save_with_before_publish(|| {
                        snapshot_ready_tx.send(()).unwrap();
                        publish_rx.recv().unwrap();
                    })
                    .unwrap();
            })
        };

        snapshot_ready_rx.recv().unwrap();
        store.purge_hosts(&HashSet::from(["pve".into()]));
        store.save().unwrap();
        publish_tx.send(()).unwrap();
        stale_saver.join().unwrap();

        let restored = RuntimeStateStore::load(path.clone()).snapshot();
        assert!(restored.hosts.is_empty());
        assert!(restored.alerts.is_empty());

        remove_file_if_present(&path);
    }

    #[test]
    fn prune_alert_states_removes_disabled_rules_and_missing_hosts() {
        let path = temporary_path("runtime-alert-prune.json");
        let store = RuntimeStateStore::load(path.clone());
        store.replace_alerts(HashMap::from([
            (
                "pve:offline".into(),
                AlertState {
                    since: 100,
                    last_enqueued_at: 131,
                },
            ),
            ("pve:disabled".into(), AlertState::default()),
            ("gone:offline".into(), AlertState::default()),
        ]));

        assert!(store.prune_alert_states(&HashSet::from(["offline".into()]), &HashSet::from(["pve".into()]),));
        assert_eq!(store.snapshot().alerts.len(), 1);
        assert!(store.snapshot().alerts.contains_key("pve:offline"));

        remove_file_if_present(&path);
    }
}
