//! The app's menu bar: the native menu on macOS, the window's menu bar on
//! Windows. Linux has no menu bar without GTK, which the app doesn't take,
//! so there the strip carries a Settings button and the Settings dialog
//! the state files ([`Platform::has_menu_bar`]).
//!
//! [`menu_bar`] says what the menus hold, as data a test reads; the native
//! menus are built from it.

/// What a menu item does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    /// Opens a state file.
    OpenState,
    /// Saves the plug-in's state as a file.
    SaveState,
    /// Opens the Settings dialog.
    Settings,
    /// Quits, saving the settings and the plug-in's state.
    Quit,
}

impl Command {
    const ALL: [Self; 4] = [Self::OpenState, Self::SaveState, Self::Settings, Self::Quit];

    /// The menu item's ID.
    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::OpenState => "lspk-open-state",
            Self::SaveState => "lspk-save-state",
            Self::Settings => "lspk-settings",
            Self::Quit => "lspk-quit",
        }
    }

    /// The command whose item has ID `id`.
    pub(crate) fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|command| command.id() == id)
    }
}

/// The platforms whose menus differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Platform {
    MacOs,
    Windows,
    Linux,
}

impl Platform {
    /// The platform this was built for.
    pub(crate) const NATIVE: Self = if cfg!(target_os = "macos") {
        Self::MacOs
    } else if cfg!(windows) {
        Self::Windows
    } else {
        Self::Linux
    };

    /// Whether the app has a menu bar here.
    pub(crate) fn has_menu_bar(self) -> bool {
        self != Self::Linux
    }
}

/// An item macOS provides in an app's menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Standard {
    About,
    Services,
    Hide,
    HideOthers,
    ShowAll,
}

/// One line of a menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    /// An item that runs `command`, with its key equivalent (with Command
    /// on macOS), if any.
    Item {
        command: Command,
        label: String,
        key: Option<char>,
    },
    /// A line between groups.
    Separator,
    /// An item the platform provides.
    Standard(Standard),
}

/// A menu in the bar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Menu {
    pub(crate) title: String,
    pub(crate) entries: Vec<Entry>,
}

fn item(command: Command, label: &str, key: Option<char>) -> Entry {
    Entry::Item {
        command,
        label: label.to_owned(),
        key,
    }
}

/// The menu bar of the app for the plug-in named `app` on `platform`;
/// empty where there's none.
pub(crate) fn menu_bar(platform: Platform, app: &str) -> Vec<Menu> {
    let open = item(Command::OpenState, "Open state…", None);
    let save = item(Command::SaveState, "Save state…", None);
    match platform {
        Platform::MacOs => vec![
            Menu {
                title: app.to_owned(),
                entries: vec![
                    Entry::Standard(Standard::About),
                    Entry::Separator,
                    item(Command::Settings, "Settings…", Some(',')),
                    Entry::Separator,
                    Entry::Standard(Standard::Services),
                    Entry::Separator,
                    Entry::Standard(Standard::Hide),
                    Entry::Standard(Standard::HideOthers),
                    Entry::Standard(Standard::ShowAll),
                    Entry::Separator,
                    item(Command::Quit, &format!("Quit {app}"), Some('q')),
                ],
            },
            Menu {
                title: "File".to_owned(),
                entries: vec![
                    item(Command::OpenState, "Open state…", Some('o')),
                    item(Command::SaveState, "Save state…", Some('s')),
                ],
            },
        ],
        // Without key equivalents: Windows' accelerators need the message
        // loop, which is baseview's.
        Platform::Windows => vec![Menu {
            title: "File".to_owned(),
            entries: vec![
                open,
                save,
                Entry::Separator,
                item(Command::Settings, "Settings…", None),
                Entry::Separator,
                item(Command::Quit, "Exit", None),
            ],
        }],
        Platform::Linux => Vec::new(),
    }
}

#[cfg(any(target_os = "macos", windows))]
pub(crate) use native::NativeMenu;

#[cfg(any(target_os = "macos", windows))]
mod native {
    use muda::accelerator::{Accelerator, Code, Modifiers};
    use muda::{AboutMetadata, IsMenuItem, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
    use raw_window_handle::HasWindowHandle;

    use super::{Command, Entry, Menu, Platform, Standard, menu_bar};

    /// The native menu bar, kept for as long as the window shows it.
    pub(crate) struct NativeMenu {
        _menu: muda::Menu,
    }

    impl NativeMenu {
        /// Builds the app's menus for the plug-in named `app` and puts them
        /// in the menu bar: the app's on macOS, `window`'s on Windows.
        pub(crate) fn install(
            app: &str,
            version: &str,
            window: &impl HasWindowHandle,
        ) -> Result<Self, String> {
            let menu = muda::Menu::new();
            for spec in menu_bar(Platform::NATIVE, app) {
                menu.append(&submenu(&spec, app, version)?)
                    .map_err(|e| e.to_string())?;
            }
            #[cfg(target_os = "macos")]
            {
                let _ = window;
                menu.init_for_nsapp();
            }
            #[cfg(windows)]
            {
                let raw_window_handle::RawWindowHandle::Win32(handle) =
                    window.window_handle().map_err(|e| e.to_string())?.as_raw()
                else {
                    return Err("the window isn't a Win32 window".to_owned());
                };
                // SAFETY: the handle is the app's own window, alive while
                // the menu is.
                unsafe { menu.init_for_hwnd(handle.hwnd.get()) }.map_err(|e| e.to_string())?;
            }
            Ok(Self { _menu: menu })
        }

        /// The commands the person has chosen from the menus since this was
        /// last called, in order.
        pub(crate) fn chosen(&self) -> Vec<Command> {
            MenuEvent::receiver()
                .try_iter()
                .filter_map(|event| Command::from_id(&event.id.0))
                .collect()
        }
    }

    fn submenu(spec: &Menu, app: &str, version: &str) -> Result<Submenu, String> {
        let items: Vec<Box<dyn IsMenuItem>> = spec
            .entries
            .iter()
            .map(|entry| -> Box<dyn IsMenuItem> {
                match entry {
                    Entry::Item {
                        command,
                        label,
                        key,
                    } => Box::new(MenuItem::with_id(
                        command.id(),
                        label,
                        true,
                        key.and_then(code)
                            .map(|code| Accelerator::new(Modifiers::META, code)),
                    )),
                    Entry::Separator => Box::new(PredefinedMenuItem::separator()),
                    Entry::Standard(Standard::About) => Box::new(PredefinedMenuItem::about(
                        Some(&format!("About {app}")),
                        Some(AboutMetadata {
                            name: Some(app.to_owned()),
                            version: Some(version.to_owned()),
                            ..AboutMetadata::default()
                        }),
                    )),
                    Entry::Standard(Standard::Services) => {
                        Box::new(PredefinedMenuItem::services(None))
                    }
                    Entry::Standard(Standard::Hide) => {
                        Box::new(PredefinedMenuItem::hide(Some(&format!("Hide {app}"))))
                    }
                    Entry::Standard(Standard::HideOthers) => {
                        Box::new(PredefinedMenuItem::hide_others(None))
                    }
                    Entry::Standard(Standard::ShowAll) => {
                        Box::new(PredefinedMenuItem::show_all(None))
                    }
                }
            })
            .collect();
        let refs: Vec<&dyn IsMenuItem> = items.iter().map(AsRef::as_ref).collect();
        Submenu::with_items(&spec.title, true, &refs).map_err(|e| e.to_string())
    }

    /// The key code for a key equivalent's character.
    fn code(key: char) -> Option<Code> {
        Some(match key {
            ',' => Code::Comma,
            'o' => Code::KeyO,
            'q' => Code::KeyQ,
            's' => Code::KeyS,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commands(menus: &[Menu]) -> Vec<(String, Command, Option<char>)> {
        menus
            .iter()
            .flat_map(|menu| {
                menu.entries.iter().filter_map(|entry| match entry {
                    Entry::Item {
                        command,
                        label,
                        key,
                    } => Some((label.clone(), *command, *key)),
                    _ => None,
                })
            })
            .collect()
    }

    #[test]
    fn macos_has_the_app_menu_and_a_file_menu() {
        let menus = menu_bar(Platform::MacOs, "Drone");
        let titles: Vec<_> = menus.iter().map(|m| m.title.as_str()).collect();
        assert_eq!(titles, ["Drone", "File"]);
        assert_eq!(
            commands(&menus),
            [
                ("Settings…".to_owned(), Command::Settings, Some(',')),
                ("Quit Drone".to_owned(), Command::Quit, Some('q')),
                ("Open state…".to_owned(), Command::OpenState, Some('o')),
                ("Save state…".to_owned(), Command::SaveState, Some('s')),
            ]
        );
        assert_eq!(menus[0].entries[0], Entry::Standard(Standard::About));
    }

    #[test]
    fn windows_has_a_file_menu_and_linux_none() {
        let menus = menu_bar(Platform::Windows, "Drone");
        assert_eq!(menus.len(), 1);
        assert_eq!(menus[0].title, "File");
        assert_eq!(
            commands(&menus),
            [
                ("Open state…".to_owned(), Command::OpenState, None),
                ("Save state…".to_owned(), Command::SaveState, None),
                ("Settings…".to_owned(), Command::Settings, None),
                ("Exit".to_owned(), Command::Quit, None),
            ]
        );
        assert!(menu_bar(Platform::Linux, "Drone").is_empty());
        assert!(!Platform::Linux.has_menu_bar());
        assert!(Platform::MacOs.has_menu_bar() && Platform::Windows.has_menu_bar());
    }

    #[test]
    fn every_command_is_found_by_its_id() {
        for command in Command::ALL {
            assert_eq!(Command::from_id(command.id()), Some(command));
        }
        assert_eq!(Command::from_id("something else"), None);
    }
}
