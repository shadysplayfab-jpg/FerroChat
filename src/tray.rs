//! Notification-area icon (Windows) and the "Start with Windows" Run key.
//! The icon exists only while Settings → Window → "Minimize to tray" is on.

use std::sync::Mutex;

use iced::futures::channel::mpsc;
use iced::futures::Stream;

#[derive(Debug, Clone)]
pub enum TrayEvent {
    Show,
    Quit,
}

#[cfg(windows)]
mod platform {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

    use super::TrayEvent;

    const SHOW: &str = "show";
    const QUIT: &str = "quit";
    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const RUN_VALUE: &str = "Ferrochat";
    /// Passed by the Run key so a login start goes straight to the tray.
    pub const MINIMIZED_ARG: &str = "--minimized";

    /// Removes the icon when dropped. Must stay on the UI thread that made it.
    pub struct Tray {
        _icon: TrayIcon,
    }

    impl std::fmt::Debug for Tray {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Tray")
        }
    }

    pub fn create() -> Result<Tray, String> {
        let menu = Menu::new();
        let show = MenuItem::with_id(SHOW, "Open Ferrochat", true, None);
        let quit = MenuItem::with_id(QUIT, "Quit", true, None);
        menu.append_items(&[&show, &PredefinedMenuItem::separator(), &quit]).map_err(|e| e.to_string())?;
        let icon = TrayIconBuilder::new()
            .with_tooltip("Ferrochat")
            .with_icon(icon()?)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Tray { _icon: icon })
    }

    /// The app logo (assets/icon.png, built into the exe).
    fn icon() -> Result<Icon, String> {
        let image = image::load_from_memory(include_bytes!("../assets/icon.png")).map_err(|e| e.to_string())?.to_rgba8();
        let (width, height) = image.dimensions();
        Icon::from_rgba(image.into_raw(), width, height).map_err(|e| e.to_string())
    }

    /// Hands tray clicks and menu choices to `send`. Set once: the tray crates
    /// keep their first handler for good.
    pub fn listen(send: fn(TrayEvent)) {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            TrayIconEvent::set_event_handler(Some(move |event| match event {
                TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. }
                | TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } => send(TrayEvent::Show),
                _ => {}
            }));
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| match event.id.as_ref() {
                SHOW => send(TrayEvent::Show),
                QUIT => send(TrayEvent::Quit),
                _ => {}
            }));
        });
    }

    /// Adds or removes this exe from the per-user Run key.
    pub fn set_autostart(on: bool) -> Result<(), String> {
        use winreg::enums::HKEY_CURRENT_USER;
        use winreg::RegKey;
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(RUN_KEY).map_err(|e| e.to_string())?;
        if on {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            key.set_value(RUN_VALUE, &format!("\"{}\" {MINIMIZED_ARG}", exe.display())).map_err(|e| e.to_string())
        } else {
            match key.delete_value(RUN_VALUE) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.to_string()),
                _ => Ok(()),
            }
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::TrayEvent;

    pub const MINIMIZED_ARG: &str = "--minimized";

    #[derive(Debug)]
    pub struct Tray;

    pub fn create() -> Result<Tray, String> {
        Err("the tray icon is only available on Windows".to_owned())
    }

    pub fn listen(_send: fn(TrayEvent)) {}

    pub fn set_autostart(_on: bool) -> Result<(), String> {
        Err("start with Windows is only available on Windows".to_owned())
    }
}

pub use platform::{create, set_autostart, Tray, MINIMIZED_ARG};

/// Where tray events go: the queue of the subscription running now. Turning
/// the tray off and on starts a new one, which takes over here, so nothing
/// left from the old one can swallow clicks.
static OUT: Mutex<Option<mpsc::Sender<TrayEvent>>> = Mutex::new(None);

/// Tray clicks and menu choices, passed on by the tray's own event handlers
/// (no threads, nothing polls).
pub fn events() -> impl Stream<Item = TrayEvent> {
    iced::stream::channel(8, |out: mpsc::Sender<TrayEvent>| async move {
        if let Ok(mut slot) = OUT.lock() {
            *slot = Some(out);
        }
        platform::listen(|event| {
            if let Ok(mut slot) = OUT.lock() {
                if let Some(out) = slot.as_mut() {
                    // A full queue only drops a click.
                    let _ = out.try_send(event);
                }
            }
        });
        std::future::pending::<()>().await;
    })
}
