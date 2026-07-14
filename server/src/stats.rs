#![allow(unused)]
use anyhow::Result;
use chrono::{Datelike, Local, Timelike};
use once_cell::sync::OnceCell;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::fs::File;
use std::io::Write;
use std::sync::mpsc::sync_channel;
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, LazyLock, Mutex};
use std::thread;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::{Host, HostGroup};
use crate::expiry;
use crate::notifier::{Event, Notifier};
use crate::payload::{HostStat, StatsResp};
use crate::runtime_state::{KnownHost, RuntimeStateStore};

const SAVE_INTERVAL: u64 = 60;
const DEFAULT_GROUP_ID: &str = "default";
const OS_LIST: [&str; 10] = [
    "centos", "debian", "ubuntu", "arch", "windows", "macos", "pi", "android", "linux", "freebsd",
];

static STAT_SENDER: OnceCell<SyncSender<Cow<HostStat>>> = OnceCell::new();

#[derive(Default)]
struct AlertEvalState {
    since: u64,
    last_sent: u64,
}

struct NotifyMessage {
    event: Event,
    stat: Arc<HostStat>,
    notification_group: String,
    notification_methods: Vec<String>,
}

impl NotifyMessage {
    fn new(event: Event, stat: Arc<HostStat>) -> Self {
        Self {
            event,
            stat,
            notification_group: String::new(),
            notification_methods: Vec::new(),
        }
    }

    fn with_rule(
        event: Event,
        stat: Arc<HostStat>,
        notification_group: String,
        notification_methods: Vec<String>,
    ) -> Self {
        Self {
            event,
            stat,
            notification_group,
            notification_methods,
        }
    }
}

pub struct StatsMgr {
    resp_json: Arc<Mutex<String>>,
    stats_data: Arc<Mutex<StatsResp>>,
    stat_map: Arc<Mutex<HashMap<String, Arc<HostStat>>>>,
    hosts_map: Arc<Mutex<HashMap<String, Host>>>,
}

impl StatsMgr {
    pub fn new() -> Self {
        Self {
            resp_json: Arc::new(Mutex::new("{}".to_string())),
            stats_data: Arc::new(Mutex::new(StatsResp::new())),
            stat_map: Arc::new(Mutex::new(HashMap::new())),
            hosts_map: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn load_last_network(hosts_map: &mut HashMap<String, Host>) {
        let contents = fs::read_to_string("stats.json").unwrap_or_default();
        if contents.is_empty() {
            return;
        }

        if let Ok(stats_json) = serde_json::from_str::<serde_json::Value>(contents.as_str()) {
            if let Some(servers) = stats_json["servers"].as_array() {
                for v in servers {
                    if let (Some(name), Some(last_network_in), Some(last_network_out)) = (
                        v["name"].as_str(),
                        v["last_network_in"].as_u64(),
                        v["last_network_out"].as_u64(),
                    ) {
                        if let Some(srv) = hosts_map.get_mut(name) {
                            srv.last_network_in = last_network_in;
                            srv.last_network_out = last_network_out;

                            trace!("{} => last in/out ({}/{}))", &name, last_network_in, last_network_out);
                        }
                    } else {
                        error!("invalid json => {v:?}");
                    }
                }
                trace!("load stats.json succ!");
            }
        } else {
            warn!("ignore invalid stats.json");
        }
    }

    fn save_stats_snapshot(resp: &StatsResp) {
        match File::create("stats.json") {
            Ok(mut file) => {
                let write_result = serde_json::to_string(resp)
                    .map_err(std::io::Error::other)
                    .and_then(|data| file.write_all(data.as_bytes()))
                    .and_then(|_| file.flush());
                if write_result.is_ok() {
                    trace!("save stats.json succ!");
                } else {
                    error!("save stats.json fail!");
                }
            }
            Err(_) => error!("save stats.json fail!"),
        }
    }

    #[allow(clippy::too_many_lines)]
    #[allow(clippy::unnecessary_wraps)]
    pub fn init(
        &mut self,
        cfg: &'static crate::config::Config,
        notifies: Arc<Mutex<Vec<Box<dyn Notifier + Send>>>>,
    ) -> Result<()> {
        let hosts_map_base = self.hosts_map.clone();
        if let Ok(mut hosts_map_guard) = hosts_map_base.lock() {
            *hosts_map_guard = cfg.hosts_map.clone();
        }

        // load last_network_in/out
        if let Ok(mut hosts_map_guard) = hosts_map_base.lock() {
            Self::load_last_network(&mut hosts_map_guard);
        }

        let runtime_state = Arc::new(RuntimeStateStore::load(
            std::path::PathBuf::from(&cfg.workspace).join("runtime-state.json"),
        ));
        let known_hosts: Vec<KnownHost> = runtime_state.snapshot().hosts.into_values().collect();
        let known_host_groups = known_hosts
            .iter()
            .filter_map(|host| {
                crate::admin::effective_group(&cfg.hosts_group_map, &host.gid)
                    .map(|group| (host.gid.clone(), group))
            })
            .collect();
        if let (Ok(mut hosts_map), Ok(mut stat_map)) = (hosts_map_base.lock(), self.stat_map.lock()) {
            restore_known_hosts(
                &mut stat_map,
                &mut hosts_map,
                &known_host_groups,
                known_hosts,
                &crate::admin::deleted_hosts(),
                crate::admin::apply_host_override,
            );
        }
        self.rebuild_cached_response();

        let (stat_tx, stat_rx) = sync_channel(512);
        STAT_SENDER.set(stat_tx).unwrap();
        let (notifier_tx, notifier_rx) = sync_channel(512);

        let stat_map = self.stat_map.clone();

        // stat_rx thread
        thread::spawn({
            let hosts_map = hosts_map_base.clone();
            let stat_map = stat_map.clone();
            let notifier_tx = notifier_tx.clone();
            let runtime_state = runtime_state.clone();

            move || {
                let mut latest_runtime_save_ts = 0_u64;
                loop {
                while let Ok(mut stat) = stat_rx.recv() {
                    trace!("recv stat `{stat:?}");

                    let mut stat_t = stat.to_mut();
                    let deleted_hosts = crate::admin::deleted_hosts();
                    if !should_process_reported_stat(stat_t, &deleted_hosts) {
                        continue;
                    }
                    if deleted_hosts.contains(&stat_t.name) {
                        if let Err(err) = crate::admin::clear_deleted_host_marker(&stat_t.name) {
                            warn!("failed to clear deleted host marker for {}: {err}", stat_t.name);
                        }
                    }

                    // group mode
                    if !stat_t.gid.is_empty() {
                        if stat_t.alias.is_empty() {
                            stat_t.alias = stat_t.name.clone();
                        }

                        if let Ok(mut hosts_map) = hosts_map.lock() {
                            let host = hosts_map.get(&stat_t.name);
                            if host.is_none() || !host.unwrap().gid.eq(&stat_t.gid) {
                                if let Some(group) = crate::admin::effective_group(&cfg.hosts_group_map, &stat_t.gid) {
                                    // 名称不变，换组了，更新组配置 & last in/out
                                    let mut inst = group.inst_host(&stat_t.name);
                                    if let Some(o) = host {
                                        inst.last_network_in = o.last_network_in;
                                        inst.last_network_out = o.last_network_out;
                                    }
                                    hosts_map.insert(stat_t.name.clone(), inst);
                                } else {
                                    continue;
                                }
                            }
                        }
                    } else if let Ok(mut hosts_map) = hosts_map.lock() {
                        assign_default_group_for_new_host(
                            stat_t,
                            &mut hosts_map,
                            crate::admin::effective_group(&cfg.hosts_group_map, DEFAULT_GROUP_ID),
                        );
                    }

                    //
                    if let Ok(mut hosts_map) = hosts_map.lock() {
                        let host_info = hosts_map.get_mut(&stat_t.name);
                        if host_info.is_none() {
                            error!("invalid stat `{stat_t:?}");
                            continue;
                        }
                        let info = host_info.unwrap();

                        if info.disabled {
                            continue;
                        }
                        crate::admin::apply_host_override(info);

                        // 补齐
                        if stat_t.location.is_empty() {
                            stat_t.location = info.location.clone();
                        }
                        if stat_t.host_type.is_empty() {
                            stat_t.host_type = info.r#type.clone();
                        }
                        stat_t.notify = info.notify && stat_t.notify;
                        stat_t.pos = info.pos;
                        stat_t.disabled = info.disabled;
                        stat_t.weight += info.weight;
                        if stat_t.gid.is_empty() {
                            stat_t.gid = info.gid.clone();
                        }
                        let public_labels = public_stat_labels(&info.labels);
                        stat_t.labels = public_labels.clone();
                        stat_t.expire = expiry::build_expire_info(&info.expire, &info.billing, &public_labels);
                        stat_t.expire_notify = info.expire_notify;

                        // !group
                        if !info.alias.is_empty() {
                            stat_t.alias = info.alias.clone();
                        }

                        info.latest_ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
                        stat_t.latest_ts = info.latest_ts;

                        // last_network_in/out
                        if !stat_t.vnstat {
                            let local_now = Local::now();
                            if info.last_network_in == 0
                                || (stat_t.network_in != 0 && info.last_network_in > stat_t.network_in)
                                || (local_now.day() == info.monthstart
                                    && local_now.hour() == 0
                                    && local_now.minute() < 5)
                            {
                                info.last_network_in = stat_t.network_in;
                                info.last_network_out = stat_t.network_out;
                            } else {
                                stat_t.last_network_in = info.last_network_in;
                                stat_t.last_network_out = info.last_network_out;
                            }
                        }

                        // uptime str
                        let day = stat_t.uptime / (3600 * 24);
                        if day > 0 {
                            stat_t.uptime_str = format!("{day} 天");
                        } else {
                            stat_t.uptime_str = format!(
                                "{:02}:{:02}:{:02}",
                                stat_t.uptime / 3600,
                                (stat_t.uptime / 60) % 60,
                                stat_t.uptime % 60
                            );
                        }

                        if let Ok(mut host_stat_map) = stat_map.lock() {
                            let is_new_host = !host_stat_map.contains_key(&stat_t.name);
                            let mut notify_up = false;
                            if let Some(pre_stat) = host_stat_map.get(&stat_t.name) {
                                if stat_t.ip_info.is_none() {
                                    stat_t.ip_info = pre_stat.ip_info.clone();
                                }

                                if stat_t.notify && (pre_stat.latest_ts + cfg.offline_threshold < stat_t.latest_ts) {
                                    notify_up = true;
                                }
                            }
                            fill_auto_location(&mut stat_t);
                            info!("update stat `{stat_t:?}");
                            let arc_stat = Arc::new(stat.into_owned());
                            if notify_up {
                                // node up notify
                                notifier_tx.send(NotifyMessage::new(Event::NodeUp, Arc::clone(&arc_stat)));
                            }
                            host_stat_map.insert(arc_stat.name.clone(), Arc::clone(&arc_stat));
                            runtime_state.upsert_host(KnownHost::from_stat(&arc_stat));
                            if is_new_host || latest_runtime_save_ts + SAVE_INTERVAL < arc_stat.latest_ts {
                                match runtime_state.save() {
                                    Ok(()) => latest_runtime_save_ts = arc_stat.latest_ts,
                                    Err(err) => warn!("failed to save runtime state: {err}"),
                                }
                            }
                            //trace!("{:?}", host_stat_map);
                        }
                    }
                }
            }
            }
        });

        // timer thread
        thread::spawn({
            let resp_json = self.resp_json.clone();
            let stats_data = self.stats_data.clone();
            let stat_map = stat_map.clone();
            let notifier_tx = notifier_tx.clone();
            let mut latest_notify_ts = 0_u64;
            let mut latest_save_ts = 0_u64;
            let mut latest_alert_check_ts = 0_u64;
            let mut expire_notify_state: HashMap<String, String> = HashMap::new();
            let mut alert_rule_state: HashMap<String, AlertEvalState> = HashMap::new();
            move || loop {
                thread::sleep(Duration::from_millis(500));

                let mut resp = StatsResp::new();
                let now = resp.updated;
                let mut any_notified = false;
                let expire_notify = crate::admin::effective_expire_notify(&cfg.expire_notify);
                let alert_rules = crate::admin::effective_alert_rules();
                let server_groups = crate::admin::snapshot().server_groups;
                let deleted_hosts = crate::admin::deleted_hosts();
                let expire_check_due = expire_notify.enabled && latest_alert_check_ts + expire_notify.interval < now;

                if let Ok(mut host_stat_map) = stat_map.lock() {
                    for (_, stat) in host_stat_map.iter_mut() {
                        if !should_publish_stat(stat, &deleted_hosts) {
                            continue;
                        }
                        if stat.disabled {
                            resp.servers.push(Arc::clone(stat));
                            continue;
                        }
                        let notify_event = {
                            let o = Arc::make_mut(stat);
                            // 30s 下线
                            mark_offline_if_stale(o, now, cfg.offline_threshold);
                            expiry::refresh_expire_info(&mut o.expire);

                            // labels
                            if !o.labels.contains("os=") {
                                if let Some(sys_info) = &o.sys_info {
                                    let os_r = sys_info.os_release.to_lowercase();
                                    for s in &OS_LIST {
                                        if os_r.contains(s) {
                                            if o.labels.is_empty() {
                                                write!(o.labels, "os={s}");
                                            } else {
                                                write!(o.labels, ";os={s}");
                                            }
                                            break;
                                        }
                                    }
                                }
                            }

                            let expire_event = if expire_check_due && o.notify && o.expire_notify {
                                if let Some(marker) = expiry::alert_marker(&o.expire, &expire_notify.days) {
                                    let should_notify = expire_notify_state.get(&o.name) != Some(&marker);
                                    expire_notify_state.insert(o.name.clone(), marker);
                                    should_notify
                                } else {
                                    expire_notify_state.remove(&o.name);
                                    false
                                }
                            } else {
                                false
                            };

                            let health_events =
                                collect_alert_events(o, now, &alert_rules, &server_groups, &mut alert_rule_state);

                            let node_event = if o.notify && latest_notify_ts + cfg.notify_interval < now {
                                if o.online4 || o.online6 {
                                    Some(Event::Custom)
                                } else {
                                    None
                                }
                            } else {
                                None
                            };

                            (node_event, expire_event, health_events)
                        };

                        // client notify — Arc::clone is O(1), no HostStat copy
                        if let Some(event) = notify_event.0 {
                            notifier_tx.send(NotifyMessage::new(event, Arc::clone(stat)));
                            any_notified = true;
                        }
                        if notify_event.1 {
                            notifier_tx.send(NotifyMessage::new(Event::Expire, Arc::clone(stat)));
                        }
                        for (health_stat, notification_group, notification_methods) in notify_event.2 {
                            notifier_tx.send(NotifyMessage::with_rule(
                                Event::Health,
                                health_stat,
                                notification_group,
                                notification_methods,
                            ));
                        }

                        resp.servers.push(Arc::clone(stat));
                    }
                    if any_notified {
                        latest_notify_ts = now;
                    }
                    if expire_check_due {
                        latest_alert_check_ts = now;
                    }
                }

                sort_servers(&mut resp.servers);

                // last_network_in/out save /60s
                if latest_save_ts + SAVE_INTERVAL < now {
                    latest_save_ts = now;
                    Self::save_stats_snapshot(&resp);
                }
                //
                if let Ok(mut o) = resp_json.lock() {
                    *o = serde_json::to_string(&resp).unwrap();
                }
                if let Ok(mut o) = stats_data.lock() {
                    *o = resp;
                }
            }
        });

        // notify thread
        thread::spawn(move || loop {
            while let Ok(msg) = notifier_rx.recv() {
                let notify_list = &*notifies.lock().unwrap();
                trace!("recv notify => {:?}, {:?}", msg.event, msg.stat);
                for n in notify_list {
                    if !crate::admin::notification_methods_allow(&msg.notification_methods, n.kind()) {
                        continue;
                    }
                    if msg.notification_methods.is_empty()
                        && !crate::admin::notification_group_allows(&msg.notification_group, n.kind())
                    {
                        continue;
                    }
                    trace!("{} notify {:?} => {:?}", n.kind(), msg.event, msg.stat);
                    n.notify(&msg.event, &msg.stat);
                }
            }
        });

        Ok(())
    }

    pub fn get_stats(&self) -> Arc<Mutex<StatsResp>> {
        self.stats_data.clone()
    }

    pub fn get_stats_json(&self) -> String {
        self.resp_json.lock().unwrap().to_string()
    }

    pub fn purge_hosts(&self, hosts: &HashSet<String>) {
        if hosts.is_empty() {
            return;
        }
        if let Ok(mut stat_map) = self.stat_map.lock() {
            stat_map.retain(|name, _| !hosts.contains(name));
        }
        if let Ok(mut hosts_map) = self.hosts_map.lock() {
            hosts_map.retain(|name, _| !hosts.contains(name));
        }
        if let Ok(mut stats_data) = self.stats_data.lock() {
            stats_data.servers.retain(|stat| !hosts.contains(&stat.name));
            if let Ok(mut resp_json) = self.resp_json.lock() {
                *resp_json = serde_json::to_string(&*stats_data).unwrap_or_else(|_| "{}".to_string());
            }
            Self::save_stats_snapshot(&stats_data);
        }
    }

    pub fn refresh_admin_overrides(&self) {
        if let (Ok(mut hosts_map), Ok(mut stat_map)) = (self.hosts_map.lock(), self.stat_map.lock()) {
            for (name, info) in hosts_map.iter_mut() {
                let previous_weight = info.weight;
                crate::admin::apply_host_override(info);
                if let Some(stat) = stat_map.get_mut(name) {
                    refresh_cached_stat_from_host(Arc::make_mut(stat), info, previous_weight);
                }
            }
        }
        self.rebuild_cached_response();
    }

    fn rebuild_cached_response(&self) {
        let deleted_hosts = crate::admin::deleted_hosts();
        let mut resp = StatsResp::new();
        if let Ok(stat_map) = self.stat_map.lock() {
            resp.servers = stat_map
                .values()
                .filter(|stat| should_publish_stat(stat, &deleted_hosts))
                .cloned()
                .collect();
        }
        sort_servers(&mut resp.servers);
        let resp_json = serde_json::to_string(&resp).unwrap_or_else(|_| "{}".to_string());
        if let Ok(mut data) = self.stats_data.lock() {
            *data = resp;
        }
        if let Ok(mut json) = self.resp_json.lock() {
            *json = resp_json;
        }
    }

    pub fn active_host_gid(&self, name: &str) -> Option<String> {
        self.stat_map
            .lock()
            .ok()
            .and_then(|stat_map| stat_map.get(name.trim()).map(|stat| stat.gid.clone()))
            .filter(|gid| !gid.trim().is_empty())
    }

    #[allow(clippy::unused_self)]
    #[allow(clippy::unnecessary_wraps)]
    pub fn report(&self, data: serde_json::Value) -> Result<()> {
        static SENDER: LazyLock<SyncSender<Cow<'static, HostStat>>> =
            LazyLock::new(|| STAT_SENDER.get().unwrap().clone());

        match serde_json::from_value(data) {
            Ok(stat) => {
                trace!("send stat => {stat:?} ");
                SENDER.send(Cow::Owned(stat));
            }
            Err(err) => {
                error!("report error => {err:?}");
            }
        }
        Ok(())
    }

    pub fn get_all_info(&self) -> Result<serde_json::Value> {
        let data = self.stats_data.lock().unwrap();
        let mut resp_json = serde_json::to_value(&*data)?;
        // for skip_serializing
        if let Some(srv_list) = resp_json["servers"].as_array_mut() {
            for (idx, stat) in data.servers.iter().enumerate() {
                if let Some(srv) = srv_list[idx].as_object_mut() {
                    srv.insert("ip_info".into(), serde_json::to_value(stat.ip_info.as_ref())?);
                    srv.insert("sys_info".into(), serde_json::to_value(stat.sys_info.as_ref())?);
                }
            }
        } else {
            // todo!()
        }

        Ok(resp_json)
    }
}

fn collect_alert_events(
    stat: &HostStat,
    now: u64,
    rules: &[crate::admin::AlertRuleOverride],
    server_groups: &[crate::admin::ServerGroupOverride],
    states: &mut HashMap<String, AlertEvalState>,
) -> Vec<(Arc<HostStat>, String, Vec<String>)> {
    if !stat.notify || rules.is_empty() {
        return Vec::new();
    }

    let online = stat.online4 || stat.online6;
    let mut events = Vec::new();
    for rule in rules {
        if !alert_rule_applies_to_stat(rule, stat, server_groups) {
            continue;
        }
        let key = format!("{}:{}", stat.name, rule.id);
        if rule.metric == "offline" {
            let state = states.entry(key).or_default();
            if online {
                state.since = 0;
                continue;
            }
            if stat.latest_ts + rule.duration < now && state.last_sent + rule.repeat_interval < now {
                state.last_sent = now;
                events.push((
                    stat_with_custom(stat, offline_alert_message(stat, rule.duration)),
                    rule.notification_group.clone(),
                    rule.notifications.clone(),
                ));
            }
            continue;
        }

        if !online {
            states.remove(&key);
            continue;
        }
        let Some(current) = metric_value(stat, &rule.metric) else {
            states.remove(&key);
            continue;
        };
        let Some(threshold) = rule.threshold else {
            states.remove(&key);
            continue;
        };
        let state = states.entry(key).or_default();
        if current > threshold {
            if state.since == 0 {
                state.since = now;
            }
            if now.saturating_sub(state.since) >= rule.duration && state.last_sent + rule.repeat_interval < now {
                state.last_sent = now;
                events.push((
                    stat_with_custom(stat, usage_alert_message(stat, rule, current, threshold)),
                    rule.notification_group.clone(),
                    rule.notifications.clone(),
                ));
            }
        } else {
            state.since = 0;
        }
    }

    events
}

fn alert_rule_applies_to_stat(
    rule: &crate::admin::AlertRuleOverride,
    stat: &HostStat,
    server_groups: &[crate::admin::ServerGroupOverride],
) -> bool {
    if rule.servers.is_empty() && rule.server_groups.is_empty() {
        return true;
    }
    if rule.servers.iter().any(|name| name == &stat.name) {
        return true;
    }
    if rule.server_groups.is_empty() {
        return false;
    }

    let selected_groups: HashSet<&str> = rule.server_groups.iter().map(String::as_str).collect();
    if !stat.gid.is_empty() && selected_groups.contains(stat.gid.as_str()) {
        return true;
    }

    server_groups
        .iter()
        .filter(|group| selected_groups.contains(group.id.as_str()))
        .any(|group| group.servers.iter().any(|name| name == &stat.name))
}

fn stat_with_custom(stat: &HostStat, custom: String) -> Arc<HostStat> {
    let mut stat = stat.clone();
    stat.custom = custom;
    Arc::new(stat)
}

fn restore_known_hosts(
    stat_map: &mut HashMap<String, Arc<HostStat>>,
    hosts_map: &mut HashMap<String, Host>,
    groups: &HashMap<String, HostGroup>,
    known_hosts: impl IntoIterator<Item = KnownHost>,
    deleted_hosts: &HashSet<String>,
    apply_override: impl Fn(&mut Host),
) {
    for known_host in known_hosts {
        if known_host.name.trim().is_empty() || deleted_hosts.contains(&known_host.name) {
            continue;
        }

        let mut stat = known_host.into_offline_stat();
        if !hosts_map.contains_key(&stat.name) {
            if let Some(group) = groups.get(&stat.gid) {
                let mut host = group.inst_host(&stat.name);
                host.latest_ts = stat.latest_ts;
                hosts_map.insert(stat.name.clone(), host);
            }
        }
        if let Some(host) = hosts_map.get_mut(&stat.name) {
            apply_override(host);
            let saved_weight = stat.weight;
            refresh_cached_stat_from_host(&mut stat, host, saved_weight);
        }
        stat.online4 = false;
        stat.online6 = false;
        stat_map.insert(stat.name.clone(), Arc::new(stat));
    }
}

fn should_process_reported_stat(stat: &HostStat, _deleted_hosts: &HashSet<String>) -> bool {
    !stat.name.trim().is_empty()
}

fn should_publish_stat(stat: &HostStat, deleted_hosts: &HashSet<String>) -> bool {
    !stat.name.trim().is_empty() && !deleted_hosts.contains(&stat.name)
}

fn mark_offline_if_stale(stat: &mut HostStat, now: u64, threshold: u64) -> bool {
    if stat.latest_ts.saturating_add(threshold) < now {
        stat.online4 = false;
        stat.online6 = false;
        return true;
    }
    false
}

fn sort_servers(servers: &mut [Arc<HostStat>]) {
    servers.sort_by(|a, b| {
        let a_online = a.online4 || a.online6;
        let b_online = b.online4 || b.online6;
        if a_online != b_online {
            return b_online.cmp(&a_online);
        }
        if a.weight != b.weight {
            return a.weight.cmp(&b.weight).reverse();
        }
        if a.pos != b.pos {
            return a.pos.cmp(&b.pos);
        }
        // same group
        a.alias.cmp(&b.alias)
    });
}

fn refresh_cached_stat_from_host(stat: &mut HostStat, info: &Host, previous_weight: u64) {
    stat.notify = info.notify && stat.notify;
    stat.pos = info.pos;
    stat.disabled = info.disabled;
    stat.weight = stat.weight.saturating_sub(previous_weight).saturating_add(info.weight);
    if stat.gid.is_empty() {
        stat.gid = info.gid.clone();
    }
    let public_labels = public_stat_labels(&info.labels);
    stat.labels = public_labels.clone();
    stat.expire = expiry::build_expire_info(&info.expire, &info.billing, &public_labels);
    stat.expire_notify = info.expire_notify;
    if !info.alias.is_empty() {
        stat.alias = info.alias.clone();
    }
    if info.location.is_empty() {
        stat.location.clear();
    } else {
        stat.location = info.location.clone();
    }
    if info.r#type.is_empty() {
        stat.host_type.clear();
    } else {
        stat.host_type = info.r#type.clone();
    }
    fill_auto_location(stat);
}

fn public_stat_labels(labels: &str) -> String {
    labels
        .split(';')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .filter(|item| item.split_once('=').is_none_or(|(key, _)| key.trim() != "note"))
        .collect::<Vec<_>>()
        .join(";")
}

fn assign_default_group_for_new_host(
    stat: &mut HostStat,
    hosts_map: &mut HashMap<String, Host>,
    default_group: Option<HostGroup>,
) -> bool {
    if !stat.gid.trim().is_empty() || stat.name.trim().is_empty() || hosts_map.contains_key(&stat.name) {
        return false;
    }

    let Some(group) = default_group else {
        return false;
    };

    stat.gid = group.gid.clone();
    let mut host = group.inst_host(&stat.name);
    if stat.alias.is_empty() {
        stat.alias = stat.name.clone();
    }
    host.latest_ts = stat.latest_ts;
    hosts_map.insert(stat.name.clone(), host);
    true
}

fn fill_auto_location(stat: &mut HostStat) {
    if stat.location.trim().is_empty() {
        if let Some(location) = infer_location_code(stat.ip_info.as_ref()) {
            stat.location = location;
        }
    }
    if stat.host_type.trim().is_empty() {
        if let Some(host_type) = infer_host_type(stat.sys_info.as_ref()) {
            stat.host_type = host_type;
        }
    }
}

fn infer_host_type(sys_info: Option<&stat_common::server_status::SysInfo>) -> Option<String> {
    let sys_info = sys_info?;
    let virtualization = sys_info.virtualization.trim().to_lowercase();
    if !virtualization.is_empty() {
        return Some(virtualization);
    }

    match sys_info.os_arch.trim().to_lowercase().as_str() {
        "aarch64" | "arm64" | "armv7" | "armv6" => Some("arm".to_string()),
        _ => Some("unknown".to_string()),
    }
}

fn infer_location_code(ip_info: Option<&stat_common::server_status::IpInfo>) -> Option<String> {
    let ip_info = ip_info?;
    country_to_code(&ip_info.country).or_else(|| timezone_to_country_code(&ip_info.timezone))
}

fn country_to_code(country: &str) -> Option<String> {
    let normalized = country
        .trim()
        .to_lowercase()
        .replace(['.', ',', '_', '-'], " ")
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ");
    if normalized.len() == 2 && normalized.chars().all(|c| c.is_ascii_alphabetic()) {
        return Some(normalized);
    }
    let code = match normalized.as_str() {
        "argentina" => "ar",
        "australia" => "au",
        "austria" => "at",
        "belgium" => "be",
        "brazil" => "br",
        "bulgaria" => "bg",
        "canada" => "ca",
        "chile" => "cl",
        "china" => "cn",
        "czechia" | "czech republic" => "cz",
        "denmark" => "dk",
        "finland" => "fi",
        "france" => "fr",
        "germany" => "de",
        "hong kong" => "hk",
        "india" => "in",
        "indonesia" => "id",
        "ireland" => "ie",
        "israel" => "il",
        "italy" => "it",
        "japan" => "jp",
        "luxembourg" => "lu",
        "macao" | "macau" => "mo",
        "malaysia" => "my",
        "mexico" => "mx",
        "netherlands" | "the netherlands" => "nl",
        "new zealand" => "nz",
        "norway" => "no",
        "philippines" => "ph",
        "poland" => "pl",
        "portugal" => "pt",
        "romania" => "ro",
        "russia" | "russian federation" => "ru",
        "singapore" => "sg",
        "south africa" => "za",
        "south korea" | "korea republic of" | "republic of korea" => "kr",
        "spain" => "es",
        "sweden" => "se",
        "switzerland" => "ch",
        "taiwan" => "tw",
        "thailand" => "th",
        "turkey" | "turkiye" => "tr",
        "ukraine" => "ua",
        "united arab emirates" => "ae",
        "united kingdom" | "great britain" | "uk" => "gb",
        "united states" | "united states of america" | "usa" => "us",
        "vietnam" | "viet nam" => "vn",
        _ => return None,
    };
    Some(code.to_string())
}

fn timezone_to_country_code(timezone: &str) -> Option<String> {
    let normalized = timezone.trim().to_lowercase();
    let code = match normalized.as_str() {
        "america/los_angeles" | "america/denver" | "america/chicago" | "america/new_york" => "us",
        "asia/hong_kong" => "hk",
        "asia/macau" => "mo",
        "asia/singapore" => "sg",
        "asia/tokyo" => "jp",
        "asia/seoul" => "kr",
        "asia/taipei" => "tw",
        "asia/shanghai" => "cn",
        "europe/london" => "gb",
        "europe/amsterdam" => "nl",
        "europe/berlin" => "de",
        "europe/paris" => "fr",
        _ => return None,
    };
    Some(code.to_string())
}

fn metric_value(stat: &HostStat, metric: &str) -> Option<f64> {
    match metric {
        "cpu" => Some(stat.cpu),
        "memory" => percent(stat.memory_used, stat.memory_total),
        "disk" => percent(stat.hdd_used, stat.hdd_total),
        "load1" => Some(stat.load_1),
        "load5" => Some(stat.load_5),
        "load15" => Some(stat.load_15),
        _ => None,
    }
}

fn percent(used: u64, total: u64) -> Option<f64> {
    if total == 0 {
        None
    } else {
        Some(used as f64 * 100.0 / total as f64)
    }
}

fn offline_alert_message(stat: &HostStat, duration: u64) -> String {
    format!(
        "节点 {} 已离线超过 {} 秒\n位置: {}\n分组: {}",
        stat.alias_or_name(),
        duration,
        empty_as_dash(&stat.location),
        empty_as_dash(&stat.gid)
    )
}

fn usage_alert_message(
    stat: &HostStat,
    rule: &crate::admin::AlertRuleOverride,
    current: f64,
    threshold: f64,
) -> String {
    format!(
        "节点 {} {} 持续超过阈值\n当前: {:.1}\n阈值: {:.1}\n持续: {} 秒",
        stat.alias_or_name(),
        metric_label(&rule.metric),
        current,
        threshold,
        rule.duration
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HostGroup;
    use crate::runtime_state::KnownHost;
    use stat_common::server_status::{IpInfo, SysInfo};

    #[test]
    fn infers_common_country_names() {
        assert_eq!(country_to_code("United States"), Some("us".to_string()));
        assert_eq!(country_to_code("Hong Kong"), Some("hk".to_string()));
        assert_eq!(country_to_code("Macao"), Some("mo".to_string()));
        assert_eq!(country_to_code("GB"), Some("gb".to_string()));
    }

    #[test]
    fn fills_empty_location_and_virtualization_type_from_telemetry() {
        let mut stat = HostStat {
            ip_info: Some(IpInfo {
                country: "United States".to_string(),
                timezone: "America/Los_Angeles".to_string(),
                ..Default::default()
            }),
            sys_info: Some(SysInfo {
                os_arch: "x86_64".to_string(),
                virtualization: "kvm".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        fill_auto_location(&mut stat);
        assert_eq!(stat.location, "us");
        assert_eq!(stat.host_type, "kvm");
    }

    #[test]
    fn falls_back_to_unknown_for_x86_without_virtualization() {
        let mut stat = HostStat {
            sys_info: Some(SysInfo {
                os_arch: "x86_64".to_string(),
                os_name: "linux".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        fill_auto_location(&mut stat);
        assert_eq!(stat.host_type, "unknown");
    }

    #[test]
    fn falls_back_to_arm_arch_as_host_type() {
        let mut stat = HostStat {
            sys_info: Some(SysInfo {
                os_arch: "aarch64".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        fill_auto_location(&mut stat);
        assert_eq!(stat.host_type, "arm");
    }

    #[test]
    fn keeps_manual_location_and_type() {
        let mut stat = HostStat {
            location: "jp".to_string(),
            host_type: "kvm".to_string(),
            ip_info: Some(IpInfo {
                country: "United States".to_string(),
                ..Default::default()
            }),
            sys_info: Some(SysInfo {
                os_arch: "aarch64".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        fill_auto_location(&mut stat);
        assert_eq!(stat.location, "jp");
        assert_eq!(stat.host_type, "kvm");
    }

    #[test]
    fn assigns_new_ungrouped_stat_to_default_group_when_available() {
        let mut hosts_map = HashMap::new();
        let mut stat = HostStat {
            name: "srv-auto".to_string(),
            ..Default::default()
        };
        let group = HostGroup {
            gid: "default".to_string(),
            password: "secret".to_string(),
            location: "hk".to_string(),
            r#type: "kvm".to_string(),
            notify: true,
            pos: 0,
            weight: 9000,
            labels: String::new(),
            expire: String::new(),
            billing: Default::default(),
            expire_notify: true,
        };

        assert!(assign_default_group_for_new_host(
            &mut stat,
            &mut hosts_map,
            Some(group)
        ));

        let host = hosts_map
            .get("srv-auto")
            .expect("host should be created from default group");
        assert_eq!(stat.gid, "default");
        assert_eq!(host.gid, "default");
        assert_eq!(host.location, "hk");
        assert_eq!(host.r#type, "kvm");
        assert_eq!(host.weight, 9000);
    }

    #[test]
    fn public_stat_labels_remove_private_note() {
        let labels = public_stat_labels("os=debian;note=private;public_note=shared;spec=2C/4G");

        assert_eq!(labels, "os=debian;public_note=shared;spec=2C/4G");
    }

    #[test]
    fn purge_hosts_removes_runtime_host_and_stat_cache() {
        let mgr = StatsMgr::new();
        mgr.hosts_map.lock().unwrap().insert(
            "srv-gone".to_string(),
            Host {
                name: "srv-gone".to_string(),
                password: "p".to_string(),
                ..Default::default()
            },
        );
        mgr.stat_map.lock().unwrap().insert(
            "srv-gone".to_string(),
            Arc::new(HostStat {
                name: "srv-gone".to_string(),
                ..Default::default()
            }),
        );
        mgr.stats_data.lock().unwrap().servers.push(Arc::new(HostStat {
            name: "srv-gone".to_string(),
            ..Default::default()
        }));

        mgr.purge_hosts(&HashSet::from(["srv-gone".to_string()]));

        assert!(!mgr.hosts_map.lock().unwrap().contains_key("srv-gone"));
        assert!(!mgr.stat_map.lock().unwrap().contains_key("srv-gone"));
        assert!(mgr.stats_data.lock().unwrap().servers.is_empty());
    }

    #[test]
    fn cached_stat_weight_refreshes_by_host_weight_delta() {
        let host = Host {
            name: "srv-weight".to_string(),
            alias: "weighted".to_string(),
            location: "hk".to_string(),
            r#type: "kvm".to_string(),
            notify: true,
            pos: 2,
            weight: 30_000,
            labels: "public_note=shared".to_string(),
            expire_notify: true,
            ..Default::default()
        };
        let mut stat = HostStat {
            name: "srv-weight".to_string(),
            alias: "old".to_string(),
            location: "us".to_string(),
            host_type: "unknown".to_string(),
            weight: 15_000,
            pos: 9,
            labels: String::new(),
            expire_notify: false,
            notify: true,
            ..Default::default()
        };

        refresh_cached_stat_from_host(&mut stat, &host, 10_000);

        assert_eq!(stat.weight, 35_000);
        assert_eq!(stat.alias, "weighted");
        assert_eq!(stat.location, "hk");
        assert_eq!(stat.host_type, "kvm");
        assert_eq!(stat.pos, 2);
        assert_eq!(stat.labels, "public_note=shared");
        assert!(stat.expire_notify);
    }

    #[test]
    fn rebuilt_cached_response_sorts_by_current_weight() {
        let mgr = StatsMgr::new();
        mgr.stat_map.lock().unwrap().insert(
            "srv-low".to_string(),
            Arc::new(HostStat {
                name: "srv-low".to_string(),
                alias: "low".to_string(),
                weight: 1_000,
                ..Default::default()
            }),
        );
        mgr.stat_map.lock().unwrap().insert(
            "srv-high".to_string(),
            Arc::new(HostStat {
                name: "srv-high".to_string(),
                alias: "high".to_string(),
                weight: 30_000,
                ..Default::default()
            }),
        );

        mgr.rebuild_cached_response();

        let names = mgr
            .stats_data
            .lock()
            .unwrap()
            .servers
            .iter()
            .map(|stat| stat.name.clone())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["srv-high".to_string(), "srv-low".to_string()]);
    }

    #[test]
    fn cached_stat_cleared_location_and_type_fall_back_to_auto_detection() {
        let host = Host {
            name: "srv-auto".to_string(),
            ..Default::default()
        };
        let mut stat = HostStat {
            name: "srv-auto".to_string(),
            location: "manual-location".to_string(),
            host_type: "manual-type".to_string(),
            ip_info: Some(stat_common::server_status::IpInfo {
                country: "Singapore".to_string(),
                ..Default::default()
            }),
            sys_info: Some(stat_common::server_status::SysInfo {
                virtualization: "kvm".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };

        refresh_cached_stat_from_host(&mut stat, &host, 0);

        assert_eq!(stat.location, "sg");
        assert_eq!(stat.host_type, "kvm");
    }

    #[test]
    fn stale_dynamic_host_is_marked_offline_but_remains_publishable() {
        let mut stat = HostStat {
            name: "pve-child".to_string(),
            gid: "default".to_string(),
            online4: true,
            online6: true,
            latest_ts: 100,
            ..Default::default()
        };

        assert!(mark_offline_if_stale(&mut stat, 131, 30));
        assert!(!stat.online4 && !stat.online6);
        assert!(should_publish_stat(&stat, &HashSet::new()));
    }

    #[test]
    fn restored_known_hosts_are_offline_filtered_and_currently_configured() {
        let mut stat_map = HashMap::new();
        let mut hosts_map = HashMap::new();
        let groups = HashMap::from([(
            "default".to_string(),
            HostGroup {
                gid: "default".to_string(),
                password: "secret".to_string(),
                location: "hk".to_string(),
                r#type: "kvm".to_string(),
                notify: true,
                pos: 2,
                weight: 9_000,
                labels: "os=debian".to_string(),
                expire: String::new(),
                billing: Default::default(),
                expire_notify: true,
            },
        )]);
        let known_hosts = vec![
            KnownHost {
                name: "pve-child".to_string(),
                alias: "PVE child".to_string(),
                gid: "default".to_string(),
                location: "old-location".to_string(),
                host_type: "old-type".to_string(),
                latest_ts: 100,
                ..Default::default()
            },
            KnownHost {
                name: "deleted".to_string(),
                ..Default::default()
            },
        ];

        restore_known_hosts(
            &mut stat_map,
            &mut hosts_map,
            &groups,
            known_hosts,
            &HashSet::from(["deleted".to_string()]),
            |_| {},
        );

        let restored = stat_map.get("pve-child").unwrap();
        assert!(!restored.online4 && !restored.online6);
        assert_eq!(restored.location, "hk");
        assert_eq!(restored.host_type, "kvm");
        assert_eq!(restored.weight, 9_000);
        assert!(!stat_map.contains_key("deleted"));
    }

    #[test]
    fn deleted_host_is_not_published_until_authenticated_report_clears_marker() {
        let deleted_hosts = HashSet::from(["srv-return".to_string()]);
        let stat = HostStat {
            name: "srv-return".to_string(),
            ..Default::default()
        };

        assert!(should_process_reported_stat(&stat, &deleted_hosts));
        assert!(!should_publish_stat(&stat, &deleted_hosts));
    }
}

trait HostStatLabel {
    fn alias_or_name(&self) -> &str;
}

impl HostStatLabel for HostStat {
    fn alias_or_name(&self) -> &str {
        if self.alias.is_empty() {
            &self.name
        } else {
            &self.alias
        }
    }
}

fn metric_label(metric: &str) -> &str {
    match metric {
        "cpu" => "CPU 使用率",
        "memory" => "内存使用率",
        "disk" => "硬盘使用率",
        "load1" => "1 分钟负载",
        "load5" => "5 分钟负载",
        "load15" => "15 分钟负载",
        _ => metric,
    }
}

fn empty_as_dash(value: &str) -> &str {
    if value.is_empty() {
        "-"
    } else {
        value
    }
}
