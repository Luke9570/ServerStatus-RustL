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

use crate::config::Host;
use crate::expiry;
use crate::notifier::{Event, Notifier};
use crate::payload::{HostStat, StatsResp};

const SAVE_INTERVAL: u64 = 60;
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
}

impl StatsMgr {
    pub fn new() -> Self {
        Self {
            resp_json: Arc::new(Mutex::new("{}".to_string())),
            stats_data: Arc::new(Mutex::new(StatsResp::new())),
            stat_map: Arc::new(Mutex::new(HashMap::new())),
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

    #[allow(clippy::too_many_lines)]
    #[allow(clippy::unnecessary_wraps)]
    pub fn init(
        &mut self,
        cfg: &'static crate::config::Config,
        notifies: Arc<Mutex<Vec<Box<dyn Notifier + Send>>>>,
    ) -> Result<()> {
        let hosts_map_base = Arc::new(Mutex::new(cfg.hosts_map.clone()));

        // load last_network_in/out
        if let Ok(mut hosts_map_guard) = hosts_map_base.lock() {
            Self::load_last_network(&mut hosts_map_guard);
        }

        let (stat_tx, stat_rx) = sync_channel(512);
        STAT_SENDER.set(stat_tx).unwrap();
        let (notifier_tx, notifier_rx) = sync_channel(512);

        let stat_map = self.stat_map.clone();

        // stat_rx thread
        thread::spawn({
            let hosts_map = hosts_map_base.clone();
            let stat_map = stat_map.clone();
            let notifier_tx = notifier_tx.clone();

            move || loop {
                while let Ok(mut stat) = stat_rx.recv() {
                    trace!("recv stat `{stat:?}");

                    let mut stat_t = stat.to_mut();
                    if crate::admin::deleted_hosts().contains(&stat_t.name) {
                        continue;
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
                        stat_t.labels = info.labels.clone();
                        stat_t.expire = expiry::build_expire_info(&info.expire, &info.billing, &info.labels);
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

                        info!("update stat `{stat_t:?}");
                        if let Ok(mut host_stat_map) = stat_map.lock() {
                            let mut notify_up = false;
                            if let Some(pre_stat) = host_stat_map.get(&stat_t.name) {
                                if stat_t.ip_info.is_none() {
                                    stat_t.ip_info = pre_stat.ip_info.clone();
                                }

                                if stat_t.notify && (pre_stat.latest_ts + cfg.offline_threshold < stat_t.latest_ts) {
                                    notify_up = true;
                                }
                            }
                            let arc_stat = Arc::new(stat.into_owned());
                            if notify_up {
                                // node up notify
                                notifier_tx.send(NotifyMessage::new(Event::NodeUp, Arc::clone(&arc_stat)));
                            }
                            host_stat_map.insert(arc_stat.name.clone(), arc_stat);
                            //trace!("{:?}", host_stat_map);
                        }
                    }
                }
            }
        });

        // timer thread
        thread::spawn({
            let resp_json = self.resp_json.clone();
            let stats_data = self.stats_data.clone();
            let hosts_map = hosts_map_base.clone();
            let stat_map = stat_map.clone();
            let notifier_tx = notifier_tx.clone();
            let mut latest_notify_ts = 0_u64;
            let mut latest_save_ts = 0_u64;
            let mut latest_group_gc = 0_u64;
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

                // group gc
                if latest_group_gc + cfg.group_gc < now {
                    latest_group_gc = now;
                    //
                    if let Ok(mut hm) = hosts_map.lock() {
                        hm.retain(|_, o| o.gid.is_empty() || o.latest_ts + cfg.group_gc >= now);
                    }
                    //
                    if let Ok(mut sm) = stat_map.lock() {
                        sm.retain(|_, o| o.gid.is_empty() || o.latest_ts + cfg.group_gc >= now);
                    }
                }

                if let Ok(mut host_stat_map) = stat_map.lock() {
                    for (_, stat) in host_stat_map.iter_mut() {
                        if deleted_hosts.contains(&stat.name) {
                            continue;
                        }
                        if stat.disabled {
                            resp.servers.push(Arc::clone(stat));
                            continue;
                        }
                        let notify_event = {
                            let o = Arc::make_mut(stat);
                            // 30s 下线
                            if o.latest_ts + cfg.offline_threshold < now {
                                o.online4 = false;
                                o.online6 = false;
                            }
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

                resp.servers.sort_by(|a, b| {
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

                // last_network_in/out save /60s
                if latest_save_ts + SAVE_INTERVAL < now {
                    latest_save_ts = now;
                    if !resp.servers.is_empty() {
                        if let Ok(mut file) = File::create("stats.json") {
                            file.write_all(serde_json::to_string(&resp).unwrap().as_bytes());
                            file.flush();
                            trace!("save stats.json succ!");
                        } else {
                            error!("save stats.json fail!");
                        }
                    }
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
        if let Ok(mut stats_data) = self.stats_data.lock() {
            stats_data.servers.retain(|stat| !hosts.contains(&stat.name));
            if let Ok(mut resp_json) = self.resp_json.lock() {
                *resp_json = serde_json::to_string(&*stats_data).unwrap_or_else(|_| "{}".to_string());
            }
        }
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
