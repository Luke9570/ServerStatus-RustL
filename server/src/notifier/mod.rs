use anyhow::Result;
use once_cell::sync::Lazy;
use serde::Serialize;
use std::sync::Mutex;
use tokio::runtime::Handle;

use crate::payload::HostStat;

pub mod bark;
pub mod email;
pub mod log;
pub mod tgbot;
pub mod webhook;
pub mod wechat;

pub static NOTIFIER_HANDLE: Lazy<Mutex<Option<Handle>>> = Lazy::new(Default::default);

#[derive(Debug, Serialize, Clone)]
pub enum Event {
    NodeUp,
    NodeDown,
    Custom,
    Expire,
    Health,
}

fn get_tag(e: &Event) -> &'static str {
    match *e {
        Event::NodeUp => "NodeUp",
        Event::NodeDown => "NodeDown",
        Event::Custom => "Custom",
        Event::Expire => "Expire",
        Event::Health => "Health",
    }
}

pub trait Notifier {
    fn kind(&self) -> &'static str;
    fn notify(&self, e: &Event, stat: &HostStat) -> Result<()>;
    // send notify impl
    fn send_notify(&self, content: String) -> Result<()>;
    fn notify_test(&self) -> Result<()> {
        self.send_notify("❗ServerStatus test msg".to_string())
    }
}

pub struct ReadinessGate<N, F> {
    inner: N,
    is_ready: F,
}

impl<N, F> ReadinessGate<N, F> {
    pub fn new(inner: N, is_ready: F) -> Self {
        Self { inner, is_ready }
    }
}

impl<N, F> Notifier for ReadinessGate<N, F>
where
    N: Notifier,
    F: Fn() -> bool,
{
    fn kind(&self) -> &'static str {
        self.inner.kind()
    }

    fn notify(&self, event: &Event, stat: &HostStat) -> Result<()> {
        if !(self.is_ready)() {
            return Ok(());
        }
        self.inner.notify(event, stat)
    }

    fn send_notify(&self, content: String) -> Result<()> {
        if !(self.is_ready)() {
            return Ok(());
        }
        self.inner.send_notify(content)
    }

    fn notify_test(&self) -> Result<()> {
        if !(self.is_ready)() {
            return Ok(());
        }
        self.inner.notify_test()
    }
}

pub struct IsolatedNotifier<N> {
    inner: N,
}

impl<N> IsolatedNotifier<N> {
    pub fn new(inner: N) -> Self {
        Self { inner }
    }
}

impl<N: Notifier> Notifier for IsolatedNotifier<N> {
    fn kind(&self) -> &'static str {
        self.inner.kind()
    }

    fn notify(&self, event: &Event, stat: &HostStat) -> Result<()> {
        if let Err(error) = self.inner.notify(event, stat) {
            log_notification_failure(self.kind(), event, &stat.name, &error);
        }
        Ok(())
    }

    fn send_notify(&self, content: String) -> Result<()> {
        self.inner.send_notify(content)
    }

    fn notify_test(&self) -> Result<()> {
        if let Err(error) = self.inner.notify_test() {
            log_notification_failure(self.kind(), &Event::Custom, "notify-test", &error);
        }
        Ok(())
    }
}

fn log_notification_failure(kind: &str, event: &Event, server: &str, error: &anyhow::Error) {
    error!(
        "notification failed: kind={kind}, event={event:?}, server={server}, error={}",
        redacted_error(error)
    );
}

fn redacted_error(_error: &anyhow::Error) -> &'static str {
    "notification failed"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };

    struct FailingNotifier;

    impl Notifier for FailingNotifier {
        fn kind(&self) -> &'static str {
            "failing"
        }

        fn notify(&self, _e: &Event, _stat: &HostStat) -> Result<()> {
            anyhow::bail!("secret provider detail")
        }

        fn send_notify(&self, _content: String) -> Result<()> {
            anyhow::bail!("secret provider detail")
        }
    }

    #[test]
    fn isolated_notifier_absorbs_provider_failure() {
        let notifier = IsolatedNotifier::new(FailingNotifier);
        let stat = HostStat {
            name: "server-one".into(),
            ..Default::default()
        };

        assert!(notifier.notify(&Event::NodeDown, &stat).is_ok());
        assert_eq!(
            redacted_error(&anyhow::anyhow!("secret provider detail")),
            "notification failed"
        );
    }

    struct CountingNotifier(Arc<AtomicUsize>);

    impl Notifier for CountingNotifier {
        fn kind(&self) -> &'static str {
            "counting"
        }

        fn notify(&self, _e: &Event, _stat: &HostStat) -> Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn send_notify(&self, _content: String) -> Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn readiness_gate_rechecks_before_each_notification() {
        let ready = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        let gate = ReadinessGate::new(CountingNotifier(Arc::clone(&calls)), {
            let ready = Arc::clone(&ready);
            move || ready.load(Ordering::SeqCst)
        });

        gate.notify(&Event::NodeDown, &HostStat::default()).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        ready.store(true, Ordering::SeqCst);
        gate.notify(&Event::NodeDown, &HostStat::default()).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
