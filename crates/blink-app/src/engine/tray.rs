//! The tray icon with its menu, and the global save hotkey.
//!
//! Both create hidden windows on the calling thread, so they must be created,
//! changed, and dropped on the tray thread, which pumps their messages.

use anyhow::Context as _;
use blink_config::Hotkey;
use blink_sys::ErrorChain;
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyManager, HotKeyState};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::icon::{ICON_SIZE, rgba};
use crate::keys::hotkey_parts;

/// What a tray menu item asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MenuAction {
    SaveClip,
    OpenClipsFolder,
    OpenLogFolder,
    Settings,
    Quit,
}

/// The tray icon and the ids of its menu items.
pub(super) struct Tray {
    icon: TrayIcon,
    items: [(MenuId, MenuAction); 5],
}

impl Tray {
    pub(super) fn new(tooltip: &str) -> anyhow::Result<Self> {
        // Clicks on the icon itself do nothing; without a handler they would
        // queue up in a channel nobody drains.
        TrayIconEvent::set_event_handler(Some(|_event: TrayIconEvent| {}));

        let save = MenuItem::new("Save clip", true, None);
        let open = MenuItem::new("Open clips folder", true, None);
        let logs = MenuItem::new("Open log folder", true, None);
        let settings = MenuItem::new("Settings", true, None);
        let quit = MenuItem::new("Quit", true, None);
        let menu = Menu::new();
        menu.append_items(&[
            &save,
            &open,
            &logs,
            &settings,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .context("failed to build the tray menu")?;

        let icon = Icon::from_rgba(rgba(), ICON_SIZE, ICON_SIZE)
            .context("failed to build the tray icon image")?;
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(tooltip)
            .with_icon(icon)
            .build()
            .context("failed to create the tray icon")?;
        Ok(Self {
            icon,
            items: [
                (save.into_id(), MenuAction::SaveClip),
                (open.into_id(), MenuAction::OpenClipsFolder),
                (logs.into_id(), MenuAction::OpenLogFolder),
                (settings.into_id(), MenuAction::Settings),
                (quit.into_id(), MenuAction::Quit),
            ],
        })
    }

    /// The next menu click, if any is queued.
    pub(super) fn next_action(&self) -> Option<MenuAction> {
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if let Some((_, action)) = self.items.iter().find(|(id, _)| *id == event.id) {
                return Some(*action);
            }
        }
        None
    }

    pub(super) fn set_tooltip(&self, tooltip: &str) {
        if let Err(error) = self.icon.set_tooltip(Some(tooltip)) {
            tracing::warn!(error = %ErrorChain(&error), "failed to update the tray tooltip");
        }
    }
}

/// The registered save hotkey.
pub(super) struct SaveHotkey {
    manager: GlobalHotKeyManager,
    registered: Option<(Hotkey, HotKey)>,
}

impl SaveHotkey {
    pub(super) fn new() -> anyhow::Result<Self> {
        Ok(Self {
            manager: GlobalHotKeyManager::new().context("failed to set up global hotkeys")?,
            registered: None,
        })
    }

    /// Registers `hotkey` in place of the current one. The new hotkey is
    /// registered first and the old one released only once that worked, so
    /// a failure (another program owns the combination, say) leaves the
    /// previous hotkey working and returns the error.
    pub(super) fn bind(&mut self, hotkey: Hotkey) -> anyhow::Result<()> {
        if self.current() == Some(hotkey) {
            return Ok(());
        }
        let (modifiers, code) = hotkey_parts(hotkey);
        let new = HotKey::new(Some(modifiers), code);
        self.manager
            .register(new)
            .with_context(|| format!("failed to register the hotkey {hotkey}"))?;
        tracing::info!(%hotkey, "save hotkey registered");
        if let Some((_, old)) = self.registered.replace((hotkey, new))
            && let Err(error) = self.manager.unregister(old)
        {
            tracing::warn!(error = %ErrorChain(&error), "failed to unregister the previous hotkey");
        }
        Ok(())
    }

    /// Unregisters the hotkey, so the settings window can capture any key
    /// combination, including the current one.
    pub(super) fn unbind(&mut self) {
        if let Some((hotkey, old)) = self.registered.take() {
            match self.manager.unregister(old) {
                Ok(()) => tracing::debug!(%hotkey, "save hotkey suspended"),
                Err(error) => {
                    tracing::warn!(error = %ErrorChain(&error), "failed to unregister the hotkey");
                }
            }
        }
    }

    /// The hotkey in effect, if any.
    pub(super) fn current(&self) -> Option<Hotkey> {
        self.registered.map(|(hotkey, _)| hotkey)
    }

    /// Whether a save press is queued. Drains every queued hotkey event.
    pub(super) fn pressed(&self) -> bool {
        let mut pressed = false;
        while let Ok(event) = global_hotkey::GlobalHotKeyEvent::receiver().try_recv() {
            let ours = self
                .registered
                .is_some_and(|(_, hotkey)| hotkey.id() == event.id());
            pressed |= ours && event.state() == HotKeyState::Pressed;
        }
        pressed
    }
}
