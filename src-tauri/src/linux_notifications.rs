//! Tracked Linux desktop notifications with cancellation across D-Bus delivery.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tauri::{AppHandle, Manager};
use zbus::blocking::{connection::Builder, Connection, MessageIterator};

const BUS: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
const DBUS: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
const METHOD_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_DELIVERIES: usize = 32;

#[derive(Clone, Copy)]
enum EarlySignal {
    Action,
    Closed,
}

#[derive(Default)]
struct DeliveryState {
    owner: Option<String>,
    generation: u64,
    in_flight: HashMap<i32, Option<String>>,
    cancelled: HashSet<i32>,
    shown: HashMap<u32, i32>,
    early: HashMap<u32, Vec<EarlySignal>>,
}

enum BindResult {
    Ready,
    Cancelled,
    Stale,
}

#[derive(Debug, PartialEq, Eq)]
enum DeliveryResult {
    Complete,
    Retry,
}

struct Finished {
    close: bool,
    click: bool,
    retry: bool,
}

impl DeliveryState {
    fn begin(&mut self, id: i32) -> Option<u64> {
        if self.in_flight.len() >= MAX_DELIVERIES {
            return None;
        }
        self.in_flight.insert(id, None);
        Some(self.generation)
    }

    fn rotate(&mut self, owner: Option<&str>) {
        if self.owner.as_deref() != owner {
            self.owner = owner.map(str::to_owned);
            self.generation = self.generation.wrapping_add(1);
            self.shown.clear();
            self.early.clear();
        }
    }

    fn bind(&mut self, id: i32, owner: &str, generation: u64) -> BindResult {
        if self.cancelled.contains(&id) {
            self.finish_without_reply(id);
            return BindResult::Cancelled;
        }
        if self.generation != generation && self.owner.as_deref() != Some(owner) {
            self.finish_without_reply(id);
            return BindResult::Stale;
        }
        self.rotate(Some(owner));
        self.in_flight.insert(id, Some(owner.to_owned()));
        BindResult::Ready
    }

    fn finish_without_reply(&mut self, id: i32) {
        self.in_flight.remove(&id);
        self.cancelled.remove(&id);
        if self.in_flight.values().all(Option::is_none) {
            self.early.clear();
        }
    }

    fn finish(&mut self, id: i32, owner: &str, server_id: u32) -> Finished {
        self.in_flight.remove(&id);
        let cancelled = self.cancelled.remove(&id);
        let stale = self.owner.as_deref() != Some(owner);
        let mut click = false;
        if !cancelled && !stale {
            self.shown.insert(server_id, id);
            if let Some(signals) = self.early.remove(&server_id) {
                for signal in signals {
                    match signal {
                        EarlySignal::Action => click = true,
                        EarlySignal::Closed => {
                            self.shown.remove(&server_id);
                        }
                    }
                }
            }
        }
        if self.in_flight.values().all(Option::is_none) {
            self.early.clear();
        }
        Finished {
            close: cancelled || stale,
            click,
            retry: stale && !cancelled,
        }
    }

    fn cancel(&mut self, ids: &[i32]) -> Vec<(String, u32)> {
        for id in ids {
            if self.in_flight.contains_key(id) {
                self.cancelled.insert(*id);
            }
        }
        let mut close = Vec::new();
        let owner = self.owner.clone();
        self.shown.retain(|server_id, id| {
            if ids.contains(id) {
                if let Some(owner) = owner.as_ref() {
                    close.push((owner.clone(), *server_id));
                }
                false
            } else {
                true
            }
        });
        close
    }

    fn signal(&mut self, sender: &str, server_id: u32, signal: EarlySignal) -> Option<i32> {
        if self.owner.as_deref() != Some(sender) {
            return None;
        }
        if let Some(id) = self.shown.get(&server_id).copied() {
            if matches!(signal, EarlySignal::Closed) {
                self.shown.remove(&server_id);
                return None;
            }
            return Some(id);
        }
        if self
            .in_flight
            .values()
            .any(|owner| owner.as_deref() == Some(sender))
            && (self.early.len() < 64 || self.early.contains_key(&server_id))
        {
            let signals = self.early.entry(server_id).or_default();
            if signals.len() < 2 {
                signals.push(signal);
            }
        }
        None
    }
}

#[derive(Default)]
pub struct LinuxNotifications {
    connection: OnceLock<Connection>,
    setup: Mutex<()>,
    state: Mutex<DeliveryState>,
}

impl LinuxNotifications {
    pub fn show(self: &Arc<Self>, app: &AppHandle, id: i32, body: String) {
        let Some(generation) = self
            .state
            .lock()
            .expect("notification state poisoned")
            .begin(id)
        else {
            retry_unavailable(app, id);
            return;
        };
        let notifier = Arc::clone(self);
        let app = app.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let connection = match notifier.connection(&app) {
                Ok(connection) => connection,
                Err(error) => {
                    notifier
                        .state
                        .lock()
                        .expect("notification state poisoned")
                        .finish_without_reply(id);
                    tracing::warn!(%error, "Failed to connect to Linux notifications");
                    retry_unavailable(&app, id);
                    return;
                }
            };
            let owner = match service_owner(&connection) {
                Ok(owner) => owner,
                Err(error) => {
                    notifier
                        .state
                        .lock()
                        .expect("notification state poisoned")
                        .finish_without_reply(id);
                    tracing::warn!(%error, "Failed to find Linux notification service");
                    retry_unavailable(&app, id);
                    return;
                }
            };
            let result = notifier.deliver_with(
                id,
                &owner,
                generation,
                || notify(&connection, &owner, &body),
                |server_id| close_server(&connection, &owner, server_id),
                |clicked| crate::attention_notifications::on_click(&app, clicked),
            );
            match result {
                Ok(DeliveryResult::Retry) => retry_unavailable(&app, id),
                Err(error) => tracing::warn!(%error, "Failed to show Linux attention notification"),
                Ok(DeliveryResult::Complete) => {}
            }
        });
    }

    pub fn close(self: &Arc<Self>, ids: Vec<i32>) {
        let server_ids = self
            .state
            .lock()
            .expect("notification state poisoned")
            .cancel(&ids);
        if server_ids.is_empty() {
            return;
        }
        let connection = self.connection.get().cloned();
        if let Some(connection) = connection {
            tauri::async_runtime::spawn_blocking(move || {
                for (owner, server_id) in server_ids {
                    if let Err(error) = close_server(&connection, &owner, server_id) {
                        tracing::warn!(%error, "Failed to close Linux attention notification");
                    }
                }
            });
        }
    }

    fn deliver_with<S, C, A>(
        &self,
        id: i32,
        owner: &str,
        generation: u64,
        send: S,
        close: C,
        click: A,
    ) -> zbus::Result<DeliveryResult>
    where
        S: FnOnce() -> zbus::Result<u32>,
        C: FnOnce(u32) -> zbus::Result<()>,
        A: FnOnce(i32),
    {
        match self
            .state
            .lock()
            .expect("notification state poisoned")
            .bind(id, owner, generation)
        {
            BindResult::Ready => {}
            BindResult::Cancelled => return Ok(DeliveryResult::Complete),
            BindResult::Stale => return Ok(DeliveryResult::Retry),
        }
        let server_id = match send() {
            Ok(server_id) => server_id,
            Err(error) => {
                let mut state = self.state.lock().expect("notification state poisoned");
                let cancelled = state.cancelled.contains(&id);
                let stale = state.owner.as_deref() != Some(owner);
                state.finish_without_reply(id);
                let timed_out = matches!(
                    &error,
                    zbus::Error::InputOutput(io) if io.kind() == std::io::ErrorKind::TimedOut
                );
                tracing::warn!(%error, "Failed to show Linux attention notification");
                return Ok(if !cancelled && (stale || !timed_out) {
                    DeliveryResult::Retry
                } else {
                    DeliveryResult::Complete
                });
            }
        };
        let finished = self
            .state
            .lock()
            .expect("notification state poisoned")
            .finish(id, owner, server_id);
        if finished.click {
            click(id);
        }
        if finished.close {
            let closed = close(server_id);
            if finished.retry {
                if let Err(error) = closed {
                    tracing::warn!(%error, "Failed to close stale Linux notification");
                }
            } else {
                closed?;
            }
        }
        Ok(if finished.retry {
            DeliveryResult::Retry
        } else {
            DeliveryResult::Complete
        })
    }

    fn connection(self: &Arc<Self>, app: &AppHandle) -> zbus::Result<Connection> {
        if let Some(connection) = self.connection.get() {
            return Ok(connection.clone());
        }
        let _setup = self.setup.lock().expect("notification setup poisoned");
        if let Some(connection) = self.connection.get() {
            return Ok(connection.clone());
        }
        let connection = Builder::session()?.method_timeout(METHOD_TIMEOUT).build()?;
        let notifications = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .path(PATH)?
            .interface(BUS)?
            .build();
        let signals = MessageIterator::for_match_rule(notifications, &connection, None)?;
        let changes = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(DBUS)?
            .interface(DBUS)?
            .member("NameOwnerChanged")?
            .add_arg(BUS)?
            .build();
        let owners = MessageIterator::for_match_rule(changes, &connection, Some(16))?;
        let notifier = Arc::clone(self);
        let app_for_owner = app.clone();
        let app = app.clone();
        std::thread::Builder::new()
            .name("clai-attention-actions".into())
            .spawn(move || {
                for signal in signals.flatten() {
                    notifier.on_signal(&app, &signal);
                }
            })
            .map_err(zbus::Error::from)?;
        let notifier = Arc::clone(self);
        std::thread::Builder::new()
            .name("clai-attention-owner".into())
            .spawn(move || {
                for signal in owners.flatten() {
                    if let Ok((name, old, new)) =
                        signal.body().deserialize::<(String, String, String)>()
                    {
                        if name == BUS {
                            {
                                let mut state = notifier.state.lock().expect("notification state poisoned");
                                if state.owner.is_none() || state.owner.as_deref() == Some(old.as_str()) {
                                    state.rotate(if new.is_empty() { None } else { Some(&new) });
                                }
                            }
                            if !new.is_empty() {
                                app_for_owner.state::<crate::attention_notifications::AttentionNotifications>().reconcile(&app_for_owner);
                            }
                        }
                    }
                }
            })
            .map_err(zbus::Error::from)?;
        let _ = self.connection.set(connection.clone());
        Ok(connection)
    }

    fn on_signal(&self, app: &AppHandle, signal: &zbus::message::Message) {
        let header = signal.header();
        let Some(sender) = header.sender().map(|sender| sender.as_str()) else {
            return;
        };
        let clicked = match header.member().map(|member| member.as_str()) {
            Some("ActionInvoked") => {
                if let Ok((server_id, action)) = signal.body().deserialize::<(u32, String)>() {
                    if action == "default" {
                        self.state
                            .lock()
                            .expect("notification state poisoned")
                            .signal(sender, server_id, EarlySignal::Action)
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            Some("NotificationClosed") => {
                if let Ok((server_id, _)) = signal.body().deserialize::<(u32, u32)>() {
                    self.state
                        .lock()
                        .expect("notification state poisoned")
                        .signal(sender, server_id, EarlySignal::Closed);
                }
                None
            }
            _ => None,
        };
        if let Some(id) = clicked {
            crate::attention_notifications::on_click(app, id);
        }
    }
}

fn retry_unavailable(app: &AppHandle, id: i32) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        app.state::<crate::attention_notifications::AttentionNotifications>()
            .delivery_unavailable(id);
        tokio::time::sleep(Duration::from_secs(2)).await;
        app.state::<crate::attention_notifications::AttentionNotifications>()
            .reconcile(&app);
    });
}

fn service_owner(connection: &Connection) -> zbus::Result<String> {
    connection
        .call_method(Some(DBUS), DBUS_PATH, Some(DBUS), "GetNameOwner", &(BUS,))?
        .body()
        .deserialize()
}

fn notify(connection: &Connection, owner: &str, body: &str) -> zbus::Result<u32> {
    let hints = HashMap::<&str, zbus::zvariant::Value<'_>>::new();
    connection
        .call_method(
            Some(owner),
            PATH,
            Some(BUS),
            "Notify",
            &(
                "CLAI",
                0u32,
                "",
                "CLAI",
                body,
                ["default", ""],
                hints,
                -1i32,
            ),
        )?
        .body()
        .deserialize()
}

fn close_server(connection: &Connection, owner: &str, server_id: u32) -> zbus::Result<()> {
    connection.call_method(
        Some(owner),
        PATH,
        Some(BUS),
        "CloseNotification",
        &(server_id,),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn cancellation_before_delivery_skips_notify() {
        let notifier = LinuxNotifications::default();
        let generation = notifier.state.lock().unwrap().begin(3).unwrap();
        assert!(notifier.state.lock().unwrap().cancel(&[3]).is_empty());
        notifier
            .deliver_with(
                3,
                ":1.1",
                generation,
                || panic!("cancelled delivery must not call Notify"),
                |_| panic!("nothing was shown to close"),
                |_| {},
            )
            .unwrap();
        assert!(notifier.state.lock().unwrap().in_flight.is_empty());
    }

    #[test]
    fn cancellation_after_delivery_returns_owner_and_server_id_for_close() {
        let notifier = LinuxNotifications::default();
        let generation = notifier.state.lock().unwrap().begin(4).unwrap();
        notifier
            .deliver_with(
                4,
                ":1.1",
                generation,
                || Ok(40),
                |_| panic!("not cancelled yet"),
                |_| {},
            )
            .unwrap();
        assert_eq!(
            notifier.state.lock().unwrap().cancel(&[4]),
            vec![(":1.1".into(), 40)]
        );
    }

    #[test]
    fn cancellation_during_blocking_delivery_closes_returned_id() {
        let notifier = Arc::new(LinuxNotifications::default());
        let generation = notifier.state.lock().unwrap().begin(7).unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (closed_tx, closed_rx) = mpsc::channel();
        let worker = {
            let notifier = Arc::clone(&notifier);
            std::thread::spawn(move || {
                notifier.deliver_with(
                    7,
                    ":1.1",
                    generation,
                    || {
                        entered_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        Ok(42)
                    },
                    |id| {
                        closed_tx.send(id).unwrap();
                        Ok(())
                    },
                    |_| {},
                )
            })
        };
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(notifier.state.lock().unwrap().cancel(&[7]).is_empty());
        release_tx.send(()).unwrap();
        worker.join().unwrap().unwrap();
        assert_eq!(closed_rx.recv().unwrap(), 42);
        assert!(notifier.state.lock().unwrap().shown.is_empty());
    }

    #[test]
    fn reply_after_old_five_second_timeout_still_closes_cancelled_notification() {
        let notifier = Arc::new(LinuxNotifications::default());
        let generation = notifier.state.lock().unwrap().begin(8).unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (closed_tx, closed_rx) = mpsc::channel();
        let worker = {
            let notifier = Arc::clone(&notifier);
            std::thread::spawn(move || {
                notifier.deliver_with(
                    8,
                    ":1.1",
                    generation,
                    || {
                        entered_tx.send(()).unwrap();
                        std::thread::sleep(Duration::from_millis(5_200));
                        Ok(43)
                    },
                    |id| {
                        closed_tx.send(id).unwrap();
                        Ok(())
                    },
                    |_| panic!("cancelled notification must not route"),
                )
            })
        };
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(notifier.state.lock().unwrap().cancel(&[8]).is_empty());
        worker.join().unwrap().unwrap();
        assert_eq!(closed_rx.recv_timeout(Duration::from_secs(1)).unwrap(), 43);
    }

    #[test]
    fn close_does_not_wait_for_connection_setup() {
        let notifier = Arc::new(LinuxNotifications::default());
        notifier.state.lock().unwrap().begin(9).unwrap();
        let setup = notifier.setup.lock().unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        let closing = Arc::clone(&notifier);
        let worker = std::thread::spawn(move || {
            closing.close(vec![9]);
            done_tx.send(()).unwrap();
        });
        let result = done_rx.recv_timeout(Duration::from_secs(1));
        drop(setup);
        worker.join().unwrap();
        result.unwrap();
        assert!(notifier.state.lock().unwrap().cancelled.contains(&9));
    }

    #[test]
    fn in_flight_cap_bounds_workers() {
        let mut state = DeliveryState::default();
        for id in 0..MAX_DELIVERIES as i32 {
            assert!(state.begin(id).is_some());
        }
        assert!(state.begin(MAX_DELIVERIES as i32).is_none());
        state.finish_without_reply(0);
        assert!(state.begin(MAX_DELIVERIES as i32).is_some());
    }

    #[test]
    fn blocked_send_does_not_block_actions_and_early_action_is_replayed() {
        let notifier = Arc::new(LinuxNotifications::default());
        let first = notifier.state.lock().unwrap().begin(1).unwrap();
        notifier
            .deliver_with(1, ":1.1", first, || Ok(41), |_| Ok(()), |_| {})
            .unwrap();
        let generation = notifier.state.lock().unwrap().begin(7).unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (clicked_tx, clicked_rx) = mpsc::channel();
        let worker = {
            let notifier = Arc::clone(&notifier);
            std::thread::spawn(move || {
                notifier.deliver_with(
                    7,
                    ":1.1",
                    generation,
                    || {
                        entered_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        Ok(42)
                    },
                    |_| Ok(()),
                    |id| clicked_tx.send(id).unwrap(),
                )
            })
        };
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        // A hung Notify cannot hold the state lock needed by the signal listener.
        let (action_tx, action_rx) = mpsc::channel();
        let signal_notifier = Arc::clone(&notifier);
        std::thread::spawn(move || {
            let mut state = signal_notifier.state.lock().unwrap();
            action_tx
                .send((
                    state.signal(":1.1", 41, EarlySignal::Action),
                    state.signal(":1.1", 42, EarlySignal::Action),
                ))
                .unwrap();
        });
        assert_eq!(
            action_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            (Some(1), None)
        );
        release_tx.send(()).unwrap();
        worker.join().unwrap().unwrap();
        assert_eq!(clicked_rx.recv_timeout(Duration::from_secs(1)).unwrap(), 7);
    }

    #[test]
    fn owner_rotation_rejects_old_actions_and_reused_ids() {
        let mut state = DeliveryState::default();
        let first = state.begin(1).unwrap();
        assert!(matches!(state.bind(1, ":1.1", first), BindResult::Ready));
        assert!(!state.finish(1, ":1.1", 42).close);
        state.rotate(Some(":1.2"));
        assert_eq!(state.signal(":1.1", 42, EarlySignal::Action), None);
        let next = state.begin(2).unwrap();
        assert!(matches!(state.bind(2, ":1.2", next), BindResult::Ready));
        assert!(!state.finish(2, ":1.2", 42).close);
        assert_eq!(state.signal(":1.1", 42, EarlySignal::Closed), None);
        assert_eq!(state.signal(":1.2", 42, EarlySignal::Action), Some(2));
        assert_eq!(state.cancel(&[2]), vec![(":1.2".into(), 42)]);
    }

    #[test]
    fn owner_rotation_before_bind_retries_without_sending() {
        let notifier = LinuxNotifications::default();
        let generation = notifier.state.lock().unwrap().begin(10).unwrap();
        notifier.state.lock().unwrap().rotate(Some(":1.2"));
        let result = notifier
            .deliver_with(
                10,
                ":1.1",
                generation,
                || panic!("stale owner must not receive Notify"),
                |_| panic!("nothing was shown"),
                |_| panic!("nothing was clicked"),
            )
            .unwrap();
        assert_eq!(result, DeliveryResult::Retry);
        assert!(notifier.state.lock().unwrap().in_flight.is_empty());
    }

    #[test]
    fn owner_rotation_before_reply_closes_old_id_then_retries() {
        let notifier = LinuxNotifications::default();
        let generation = notifier.state.lock().unwrap().begin(11).unwrap();
        let result = notifier
            .deliver_with(
                11,
                ":1.1",
                generation,
                || {
                    notifier.state.lock().unwrap().rotate(Some(":1.2"));
                    Ok(44)
                },
                |server_id| {
                    assert_eq!(server_id, 44);
                    Ok(())
                },
                |_| panic!("old owner click must not route"),
            )
            .unwrap();
        assert_eq!(result, DeliveryResult::Retry);
        assert!(notifier.state.lock().unwrap().shown.is_empty());
    }

    #[test]
    fn owner_rotation_during_failed_notify_retries() {
        let notifier = LinuxNotifications::default();
        let generation = notifier.state.lock().unwrap().begin(12).unwrap();
        let result = notifier
            .deliver_with(
                12,
                ":1.1",
                generation,
                || {
                    notifier.state.lock().unwrap().rotate(Some(":1.2"));
                    Err(zbus::Error::Failure("old owner disappeared".into()))
                },
                |_| panic!("Notify returned no ID to close"),
                |_| panic!("failed notification must not route"),
            )
            .unwrap();
        assert_eq!(result, DeliveryResult::Retry);
        let state = notifier.state.lock().unwrap();
        assert!(state.in_flight.is_empty());
        assert!(state.shown.is_empty());
        assert_eq!(state.owner.as_deref(), Some(":1.2"));
    }

    #[test]
    fn transient_notify_error_retries_but_cancelled_error_does_not() {
        let notifier = LinuxNotifications::default();
        let generation = notifier.state.lock().unwrap().begin(13).unwrap();
        let failed = notifier
            .deliver_with(
                13,
                ":1.1",
                generation,
                || Err(zbus::Error::Failure("transient failure".into())),
                |_| panic!("Notify returned no ID to close"),
                |_| panic!("failed notification must not route"),
            )
            .unwrap();
        assert_eq!(failed, DeliveryResult::Retry);

        let generation = notifier.state.lock().unwrap().begin(14).unwrap();
        let cancelled = notifier
            .deliver_with(
                14,
                ":1.1",
                generation,
                || {
                    notifier.state.lock().unwrap().cancel(&[14]);
                    Err(zbus::Error::Failure("cancelled failure".into()))
                },
                |_| panic!("Notify returned no ID to close"),
                |_| panic!("failed notification must not route"),
            )
            .unwrap();
        assert_eq!(cancelled, DeliveryResult::Complete);
        let state = notifier.state.lock().unwrap();
        assert!(state.in_flight.is_empty());
        assert!(state.cancelled.is_empty());
    }

    #[test]
    fn unanswered_notify_does_not_retry_current_owner() {
        let notifier = LinuxNotifications::default();
        let generation = notifier.state.lock().unwrap().begin(15).unwrap();
        let result = notifier
            .deliver_with(
                15,
                ":1.1",
                generation,
                || {
                    Err(zbus::Error::InputOutput(std::sync::Arc::new(
                        std::io::Error::new(std::io::ErrorKind::TimedOut, "no reply"),
                    )))
                },
                |_| panic!("Notify returned no ID to close"),
                |_| panic!("unanswered notification must not route"),
            )
            .unwrap();
        assert_eq!(result, DeliveryResult::Complete);
        assert!(notifier.state.lock().unwrap().in_flight.is_empty());
    }

    #[test]
    fn owner_rotation_during_send_closes_only_old_owner_id() {
        let mut state = DeliveryState::default();
        let generation = state.begin(1).unwrap();
        assert!(matches!(
            state.bind(1, ":1.1", generation),
            BindResult::Ready
        ));
        state.rotate(Some(":1.2"));
        assert!(state.finish(1, ":1.1", 42).close);
        assert!(state.shown.is_empty());
    }
}
