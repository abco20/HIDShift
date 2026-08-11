use tauri::AppHandle;
use tokio::sync::mpsc;
use tokio::time::Duration;

const APP_NAME: &str = "hidshift-companion";
const APP_ICON: &str = "hidshift-companion";
const TITLE: &str = "HIDShift";
const NOTIFICATION_QUEUE_CAPACITY: usize = 8;
const NOTIFICATION_DISPLAY_DURATION: Duration = Duration::from_secs(2);

trait NotificationTransport {
    async fn show(&self, body: &str, display_duration: Duration) -> Result<(), String>;
}

trait NotificationConnector {
    type Transport: NotificationTransport;

    async fn connect(&self) -> Result<Self::Transport, String>;
}

struct PersistentNotifier<C: NotificationConnector> {
    connector: C,
    transport: Option<C::Transport>,
}

impl<C: NotificationConnector> PersistentNotifier<C> {
    const fn new(connector: C) -> Self {
        Self {
            connector,
            transport: None,
        }
    }

    async fn show(&mut self, body: &str) -> Result<(), String> {
        if self.transport.is_none() {
            self.transport = Some(self.connector.connect().await?);
        }
        let Some(transport) = self.transport.as_ref() else {
            return Err("notification transport initialization failed".into());
        };
        let result = transport.show(body, NOTIFICATION_DISPLAY_DURATION).await;
        if result.is_err() {
            self.transport = None;
        }
        result
    }
}

#[derive(Clone)]
pub struct DesktopNotifier {
    sender: mpsc::Sender<String>,
}

impl DesktopNotifier {
    pub fn new(app: AppHandle) -> Self {
        let (sender, receiver) = mpsc::channel(NOTIFICATION_QUEUE_CAPACITY);
        spawn_notification_task(app, receiver);
        Self { sender }
    }

    pub fn show(&self, body: String) {
        let _ = self.sender.try_send(body);
    }
}

#[cfg(target_os = "linux")]
fn spawn_notification_task(_app: AppHandle, mut receiver: mpsc::Receiver<String>) {
    tauri::async_runtime::spawn(async move {
        let mut notifier = PersistentNotifier::new(LinuxNotificationConnector);
        while let Some(body) = receiver.recv().await {
            if let Err(error) = notifier.show(&body).await {
                eprintln!("desktop notification failed: {error}");
            }
        }
    });
}

#[cfg(not(target_os = "linux"))]
fn spawn_notification_task(app: AppHandle, mut receiver: mpsc::Receiver<String>) {
    use tauri_plugin_notification::NotificationExt;

    tauri::async_runtime::spawn(async move {
        while let Some(body) = receiver.recv().await {
            let _ = app.notification().builder().title(TITLE).body(body).show();
        }
    });
}

#[cfg(target_os = "linux")]
struct LinuxNotificationConnector;

#[cfg(target_os = "linux")]
struct LinuxNotificationTransport {
    connection: zbus::Connection,
}

#[cfg(target_os = "linux")]
impl NotificationConnector for LinuxNotificationConnector {
    type Transport = LinuxNotificationTransport;

    async fn connect(&self) -> Result<Self::Transport, String> {
        zbus::Connection::session()
            .await
            .map(|connection| LinuxNotificationTransport { connection })
            .map_err(|error| error.to_string())
    }
}

#[cfg(target_os = "linux")]
impl NotificationTransport for LinuxNotificationTransport {
    async fn show(&self, body: &str, display_duration: Duration) -> Result<(), String> {
        let proxy = zbus::Proxy::new(
            &self.connection,
            "org.freedesktop.Notifications",
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
        )
        .await
        .map_err(|error| error.to_string())?;
        let actions = Vec::<&str>::new();
        let hints = std::collections::HashMap::<&str, zbus::zvariant::OwnedValue>::new();
        let expire_timeout = i32::try_from(display_duration.as_millis()).unwrap_or(i32::MAX);
        let notification_id: u32 = proxy
            .call(
                "Notify",
                &(
                    APP_NAME,
                    0u32,
                    APP_ICON,
                    TITLE,
                    body,
                    actions,
                    hints,
                    expire_timeout,
                ),
            )
            .await
            .map_err(|error| error.to_string())?;

        // GNOME Shell 46 ignores the freedesktop expire_timeout argument. Close the
        // notification explicitly while retaining the owned D-Bus connection; losing
        // that connection would make GNOME discard the notification immediately.
        let connection = self.connection.clone();
        tokio::spawn(async move {
            tokio::time::sleep(display_duration).await;
            let Ok(proxy) = zbus::Proxy::new(
                &connection,
                "org.freedesktop.Notifications",
                "/org/freedesktop/Notifications",
                "org.freedesktop.Notifications",
            )
            .await
            else {
                return;
            };
            let _: Result<(), zbus::Error> =
                proxy.call("CloseNotification", &(notification_id,)).await;
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone)]
    struct MockConnector {
        connections: Arc<AtomicUsize>,
        deliveries: Arc<AtomicUsize>,
        display_duration_millis: Arc<AtomicUsize>,
    }

    struct MockTransport {
        deliveries: Arc<AtomicUsize>,
        display_duration_millis: Arc<AtomicUsize>,
    }

    impl NotificationConnector for MockConnector {
        type Transport = MockTransport;

        async fn connect(&self) -> Result<Self::Transport, String> {
            self.connections.fetch_add(1, Ordering::Relaxed);
            Ok(MockTransport {
                deliveries: Arc::clone(&self.deliveries),
                display_duration_millis: Arc::clone(&self.display_duration_millis),
            })
        }
    }

    impl NotificationTransport for MockTransport {
        async fn show(&self, _body: &str, display_duration: Duration) -> Result<(), String> {
            self.deliveries.fetch_add(1, Ordering::Relaxed);
            self.display_duration_millis.store(
                usize::try_from(display_duration.as_millis()).unwrap(),
                Ordering::Relaxed,
            );
            Ok(())
        }
    }

    #[tokio::test]
    async fn repeated_notifications_reuse_one_owned_connection() {
        let connections = Arc::new(AtomicUsize::new(0));
        let deliveries = Arc::new(AtomicUsize::new(0));
        let display_duration_millis = Arc::new(AtomicUsize::new(0));
        let connector = MockConnector {
            connections: Arc::clone(&connections),
            deliveries: Arc::clone(&deliveries),
            display_duration_millis: Arc::clone(&display_duration_millis),
        };
        let mut notifier = PersistentNotifier::new(connector);

        notifier.show("first").await.unwrap();
        notifier.show("second").await.unwrap();

        assert_eq!(connections.load(Ordering::Relaxed), 1);
        assert_eq!(deliveries.load(Ordering::Relaxed), 2);
        assert_eq!(display_duration_millis.load(Ordering::Relaxed), 2_000);
    }
}
