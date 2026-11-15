//! Linux tray icon: a StatusNotifierItem (the freedesktop tray protocol) over
//! D-Bus, through ksni, which runs it on its own thread.

use futures::channel::mpsc::UnboundedSender;
use ksni::blocking::TrayMethods as _;
use ksni::menu::{MenuItem, StandardItem};

use super::{Command, Status, menu_labels};

pub struct Tray {
    handle: ksni::blocking::Handle<Item>,
}

struct Item {
    commands: UnboundedSender<Command>,
    status: Status,
    icons: Vec<ksni::Icon>,
}

impl Item {
    fn send(&self, c: Command) {
        let _ = self.commands.unbounded_send(c);
    }
}

impl Tray {
    pub fn start(commands: UnboundedSender<Command>) -> Option<Self> {
        // Panels pick the size closest to theirs.
        let icons = [22, 32, 48, 64]
            .into_iter()
            .map(|size| {
                // ARGB, big-endian.
                let data = super::icon_rgba(size).chunks_exact(4).flat_map(|p| [p[3], p[0], p[1], p[2]]).collect();
                ksni::Icon { width: size as i32, height: size as i32, data }
            })
            .collect();
        match (Item { commands, status: Status::default(), icons }).spawn() {
            Ok(handle) => Some(Self { handle }),
            Err(e) => {
                // Usually no tray on this desktop (GNOME without the extension).
                eprintln!("no system tray: {e}");
                None
            }
        }
    }

    pub fn set_status(&self, status: &Status) {
        let status = status.clone();
        self.handle.update(move |item| item.status = status);
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        let _ = self.handle.shutdown();
    }
}

impl ksni::Tray for Item {
    fn id(&self) -> String {
        "local-playlist".into()
    }

    fn title(&self) -> String {
        "Local Playlist".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::ApplicationStatus
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icons.clone()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "Local Playlist".into(),
            description: self.status.track.clone().unwrap_or_default(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(Command::ToggleWindow);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let now = StandardItem {
            label: self.status.track.clone().unwrap_or_else(|| "Nothing playing".into()),
            enabled: false,
            ..Default::default()
        };
        let mut items = vec![now.into(), MenuItem::Separator];
        for (i, (command, label)) in menu_labels(&self.status).into_iter().enumerate() {
            if i == 3 {
                items.push(MenuItem::Separator);
            }
            let item = StandardItem {
                label: label.into(),
                activate: Box::new(move |item: &mut Self| item.send(command)),
                ..Default::default()
            };
            items.push(item.into());
        }
        items
    }

    fn watcher_online(&self) {
        self.send(Command::TrayBack);
    }

    fn watcher_offline(&self, _: ksni::OfflineReason) -> bool {
        self.send(Command::TrayGone);
        // Keep the item, to show again when the tray comes back.
        true
    }
}
