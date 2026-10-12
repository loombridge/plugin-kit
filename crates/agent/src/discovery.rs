//! The discovery file: how an agent finds the running programs, such as
//! plug-in instances, whose agent servers are on.
//!
//! Each running instance with its server on has an entry in one file in a
//! per-user folder ([`user_dir`]): `instances.json`, beside lock files of
//! its own. An entry holds the instance's address and token, the program's
//! ID and name, and, for a plug-in, the host's name and the name of the
//! track it's on where the host reports it, so an agent can choose an
//! instance and connect to it:
//!
//! ```json
//! { "version": 1, "instances": [ {
//!   "instance": "9f2c41d07a5be318", "pid": 4312,
//!   "id": "com.loombridge.example.gain", "name": "Example Gain",
//!   "url": "http://127.0.0.1:50123/mcp", "port": 50123, "token": "…",
//!   "host": "Bitwig Studio", "track": "Bass", "started": 1791820800 } ] }
//! ```
//!
//! # The token is in the file
//!
//! An entry carries the instance's bearer token, so an agent the person
//! runs can connect without the person copying it across. The file is
//! therefore readable by the user alone: on macOS and Linux the folder is
//! made `0700` and the file `0600`; on Windows it lives in the user's own
//! `%APPDATA%`, which only the user (and administrators) can read. Every
//! other check still applies: the server listens on `127.0.0.1` only, and
//! refuses web pages ([`http`](crate::http)).
//!
//! # Entries come and go
//!
//! [`Discovery::register`] adds an entry and returns a [`Registration`];
//! dropping it removes the entry. An instance that dies without removing
//! its entry leaves it behind, so every entry has a lock file
//! (`<instance>.alive`) that its instance holds locked for as long as it
//! runs; the operating system lets go of the lock when the process ends.
//! [`Discovery::entries`] ignores an entry whose lock nobody holds, and the
//! next change to the file drops it. Changes take a lock on
//! `instances.lock`, read the file, change it and replace it whole, so
//! instances in several processes never lose each other's entries, and a
//! reader never sees half a file.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// The discovery file's name, in its folder.
pub const FILE: &str = "instances.json";
/// The version of its layout.
pub const VERSION: u64 = 1;
/// The lock file changes take.
const LOCK: &str = "instances.lock";

/// The per-user folder the discovery file is in: `lspk/agents` inside
/// `~/Library/Application Support` on macOS, `%APPDATA%` on Windows, and on
/// Linux and other Unix systems `$XDG_RUNTIME_DIR`, `$XDG_DATA_HOME` or
/// `~/.local/share`, the first that's set. `None` if none of them is.
///
/// A sandboxed host (an out-of-process Audio Unit, say) may give a plug-in
/// a home folder of its own, so its entries land in its container.
#[must_use]
pub fn user_dir() -> Option<PathBuf> {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    #[cfg(target_os = "macos")]
    let base = var("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"));
    #[cfg(windows)]
    let base = var("APPDATA").map(PathBuf::from);
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = var("XDG_RUNTIME_DIR")
        .or_else(|| var("XDG_DATA_HOME"))
        .map(PathBuf::from)
        .or_else(|| var("HOME").map(|home| PathBuf::from(home).join(".local/share")));
    #[cfg(not(any(unix, windows)))]
    let base: Option<PathBuf> = None;
    Some(base?.join("lspk").join("agents"))
}

/// One running instance's entry.
#[derive(Clone, PartialEq, Eq)]
pub struct Entry {
    /// The instance's own ID, which names its lock file. [`Discovery::register`]
    /// sets it.
    pub instance: String,
    /// The process it runs in. [`Discovery::register`] sets it.
    pub pid: u32,
    /// The program's ID, such as a plug-in's `com.loombridge.reagent`.
    pub id: String,
    /// The program's name, such as `Reagent`.
    pub name: String,
    /// Where its MCP endpoint is, such as `http://127.0.0.1:50123/mcp`.
    pub url: String,
    /// The port it listens on, on `127.0.0.1`.
    pub port: u16,
    /// The bearer token its requests must carry.
    pub token: String,
    /// The host's name, for a plug-in, if the host gives one.
    pub host: Option<String>,
    /// The name of the track a plug-in is on, if the host reports it.
    pub track: Option<String>,
    /// When it started, in seconds since 1970.
    pub started: u64,
}

impl std::fmt::Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the token.
        f.debug_struct("Entry")
            .field("instance", &self.instance)
            .field("pid", &self.pid)
            .field("id", &self.id)
            .field("name", &self.name)
            .field("url", &self.url)
            .field("host", &self.host)
            .field("track", &self.track)
            .finish_non_exhaustive()
    }
}

impl Entry {
    /// An entry for program `id`, called `name`, serving at `url` on `port`
    /// with `token`, started now.
    #[must_use]
    pub fn new(id: &str, name: &str, url: &str, port: u16, token: &str) -> Self {
        Self {
            instance: String::new(),
            pid: std::process::id(),
            id: id.to_owned(),
            name: name.to_owned(),
            url: url.to_owned(),
            port,
            token: token.to_owned(),
            host: None,
            track: None,
            started: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "instance": self.instance,
            "pid": self.pid,
            "id": self.id,
            "name": self.name,
            "url": self.url,
            "port": self.port,
            "token": self.token,
            "host": self.host,
            "track": self.track,
            "started": self.started,
        })
    }

    fn from_json(value: &Value) -> Option<Self> {
        let text = |key: &str| value.get(key)?.as_str().map(str::to_owned);
        Some(Self {
            instance: text("instance")?,
            pid: u32::try_from(value.get("pid")?.as_u64()?).ok()?,
            id: text("id")?,
            name: text("name")?,
            url: text("url")?,
            port: u16::try_from(value.get("port")?.as_u64()?).ok()?,
            token: text("token")?,
            host: text("host"),
            track: text("track"),
            started: value.get("started").and_then(Value::as_u64).unwrap_or(0),
        })
    }
}

/// A discovery folder: where entries are written and read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Discovery {
    dir: PathBuf,
}

impl Discovery {
    /// The per-user folder ([`user_dir`]), if there is one.
    #[must_use]
    pub fn user() -> Option<Self> {
        user_dir().map(Self::at)
    }

    /// The folder `dir`, for tests and programs with a folder of their own.
    #[must_use]
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The folder.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The discovery file.
    #[must_use]
    pub fn file(&self) -> PathBuf {
        self.dir.join(FILE)
    }

    /// Every entry whose instance still runs.
    ///
    /// # Errors
    ///
    /// The file exists but can't be read. A missing file has no entries.
    pub fn entries(&self) -> io::Result<Vec<Entry>> {
        let mut entries = self.read()?;
        entries.retain(|entry| self.alive(entry));
        Ok(entries)
    }

    /// Adds `entry`, with a new instance ID and this process's ID, and
    /// returns the registration that keeps it; dropping that removes it.
    ///
    /// # Errors
    ///
    /// The folder or its files couldn't be made or written.
    pub fn register(&self, mut entry: Entry) -> io::Result<Registration> {
        self.make_dir()?;
        entry.instance = new_instance_id()?;
        entry.pid = std::process::id();
        let alive = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.alive_path(&entry.instance))?;
        alive.try_lock().map_err(|error| match error {
            TryLockError::Error(error) => error,
            TryLockError::WouldBlock => io::Error::other("the instance's lock is taken"),
        })?;
        let added = entry.clone();
        self.change(|entries| entries.push(added))?;
        Ok(Registration {
            discovery: self.clone(),
            entry,
            alive: Some(alive),
        })
    }

    /// Whether `entry`'s instance still holds its lock.
    fn alive(&self, entry: &Entry) -> bool {
        let Ok(file) = File::open(self.alive_path(&entry.instance)) else {
            return false;
        };
        match file.try_lock() {
            Ok(()) => false,
            Err(TryLockError::WouldBlock) => true,
            // A lock that can't be tried says nothing; keep the entry.
            Err(TryLockError::Error(_)) => true,
        }
    }

    fn alive_path(&self, instance: &str) -> PathBuf {
        self.dir.join(format!("{instance}.alive"))
    }

    fn make_dir(&self) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    fn read(&self) -> io::Result<Vec<Entry>> {
        let text = match fs::read_to_string(self.file()) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        // A file this version can't read is replaced at the next change.
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        Ok(value
            .get("instances")
            .and_then(Value::as_array)
            .map(|list| list.iter().filter_map(Entry::from_json).collect())
            .unwrap_or_default())
    }

    /// Under the folder's lock: reads the entries, drops those whose
    /// instance has gone (and their lock files), applies `change`, and
    /// replaces the file.
    fn change(&self, change: impl FnOnce(&mut Vec<Entry>)) -> io::Result<()> {
        self.make_dir()?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.dir.join(LOCK))?;
        lock.lock()?;
        let mut entries = self.read()?;
        entries.retain(|entry| {
            let alive = self.alive(entry);
            if !alive {
                let _ = fs::remove_file(self.alive_path(&entry.instance));
            }
            alive
        });
        change(&mut entries);
        let body = json!({
            "version": VERSION,
            "instances": entries.iter().map(Entry::to_json).collect::<Vec<_>>(),
        });
        let temporary = self.dir.join(format!("{FILE}.{}.tmp", std::process::id()));
        let mut options = OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(serde_json::to_string_pretty(&body)?.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, self.file())?;
        drop(lock);
        Ok(())
    }
}

/// An instance's entry in the discovery file, for as long as it's kept;
/// dropping it removes the entry.
#[derive(Debug)]
pub struct Registration {
    discovery: Discovery,
    entry: Entry,
    alive: Option<File>,
}

impl Registration {
    /// The entry as written.
    #[must_use]
    pub fn entry(&self) -> &Entry {
        &self.entry
    }

    /// Changes the entry (its track's name, say) and writes it.
    ///
    /// # Errors
    ///
    /// The file couldn't be written.
    pub fn update(&mut self, change: impl FnOnce(&mut Entry)) -> io::Result<()> {
        let mut entry = self.entry.clone();
        change(&mut entry);
        entry.instance.clone_from(&self.entry.instance);
        if entry == self.entry {
            return Ok(());
        }
        let written = entry.clone();
        self.discovery.change(|entries| {
            match entries.iter_mut().find(|e| e.instance == written.instance) {
                Some(slot) => *slot = written,
                None => entries.push(written),
            }
        })?;
        self.entry = entry;
        Ok(())
    }

    /// Removes the entry now.
    ///
    /// # Errors
    ///
    /// The file couldn't be written; the entry is ignored all the same, as
    /// its instance no longer holds its lock.
    pub fn remove(mut self) -> io::Result<()> {
        self.unregister()
    }

    fn unregister(&mut self) -> io::Result<()> {
        let Some(alive) = self.alive.take() else {
            return Ok(());
        };
        let instance = self.entry.instance.clone();
        let result = self
            .discovery
            .change(|entries| entries.retain(|e| e.instance != instance));
        drop(alive);
        let _ = fs::remove_file(self.discovery.alive_path(&instance));
        result
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        let _ = self.unregister();
    }
}

/// A new instance ID: 8 random bytes as 16 hexadecimal digits.
fn new_instance_id() -> io::Result<String> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A discovery folder of the test's own, removed afterwards.
    struct Folder(Discovery);

    impl Folder {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "lspk-discovery-{}",
                new_instance_id().expect("random bytes")
            ));
            Self(Discovery::at(dir))
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(self.0.dir());
        }
    }

    fn entry(port: u16) -> Entry {
        Entry::new(
            "com.example.synth",
            "Synth",
            &format!("http://127.0.0.1:{port}/mcp"),
            port,
            "secret",
        )
    }

    #[test]
    fn entries_come_and_go_with_their_registrations() {
        let folder = Folder::new();
        let discovery = &folder.0;
        assert_eq!(discovery.entries().expect("no file yet"), []);
        let first = discovery.register(entry(5001)).expect("registered");
        let mut second = discovery.register(entry(5002)).expect("registered");
        assert_ne!(first.entry().instance, second.entry().instance);
        let entries = discovery.entries().expect("entries");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].pid, std::process::id());
        assert_eq!(entries[1].token, "secret");

        second
            .update(|e| {
                e.host = Some("Host".to_owned());
                e.track = Some("Bass".to_owned());
            })
            .expect("updated");
        let found = discovery.entries().expect("entries");
        assert_eq!(found[1].track.as_deref(), Some("Bass"));
        assert_eq!(found[1].host.as_deref(), Some("Host"));

        drop(first);
        let entries = discovery.entries().expect("entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].port, 5002);
        second.remove().expect("removed");
        assert_eq!(discovery.entries().expect("entries"), []);
        assert!(!format!("{:?}", entry(1)).contains("secret"));
    }

    #[test]
    fn an_entry_whose_instance_has_gone_is_ignored_and_dropped() {
        let folder = Folder::new();
        let discovery = &folder.0;
        let live = discovery.register(entry(5003)).expect("registered");
        // An instance that died: its entry stays, but nobody holds its lock.
        let mut dead = entry(5004);
        dead.instance = "deadbeefdeadbeef".to_owned();
        File::create(discovery.alive_path(&dead.instance)).expect("a lock file");
        discovery
            .change(|entries| entries.push(dead))
            .expect("written");
        let raw = fs::read_to_string(discovery.file()).expect("the file");
        assert!(raw.contains("deadbeefdeadbeef"));
        let entries = discovery.entries().expect("entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].instance, live.entry().instance);
        // The next change drops it, lock file and all.
        let other = discovery.register(entry(5005)).expect("registered");
        let raw = fs::read_to_string(discovery.file()).expect("the file");
        assert!(!raw.contains("deadbeefdeadbeef"));
        assert!(!discovery.alive_path("deadbeefdeadbeef").exists());
        drop((live, other));
    }

    #[cfg(unix)]
    #[test]
    fn only_the_user_can_read_it() {
        use std::os::unix::fs::PermissionsExt;
        let folder = Folder::new();
        let discovery = &folder.0;
        let _kept = discovery.register(entry(5006)).expect("registered");
        let mode = |path: &Path| fs::metadata(path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode(&discovery.file()), 0o600);
        assert_eq!(mode(discovery.dir()), 0o700);
    }

    #[test]
    fn the_user_folder_is_per_user() {
        if let Some(dir) = user_dir() {
            assert!(dir.ends_with(Path::new("lspk").join("agents")), "{dir:?}");
        }
    }
}
