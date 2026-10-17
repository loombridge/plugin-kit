//! The agent server's life inside a plug-in instance: started and stopped
//! on the main thread as the person's setting says, served from
//! [`Plugin::on_main_thread`], and listed in the discovery file while it
//! runs.

use std::sync::Arc;

use lspk_agent::discovery::{Discovery, Entry, Registration};
use lspk_agent::http::{self, Endpoint, HttpConfig, HttpServer, Parked, Pending, Served};
use lspk_agent::permission::Mode;
use lspk_agent::{Question, Server};
use lspk_plugin::{Host, Plugin};

use crate::commands::ServerStatus;
use crate::tools::Tools;

/// The path agents post to.
pub const PATH: &str = "/mcp";

/// A running server, and what waits on it.
struct Running {
    http: HttpServer,
    token: String,
    /// Calls waiting for a preview to render.
    parked: Vec<Parked>,
    /// Calls waiting for the person's answer.
    pending: Vec<Pending>,
    registration: Option<Registration>,
    /// The discovery file the entry is in, or why there's no entry.
    discovery: Result<String, String>,
}

/// The agent interface of one plug-in instance.
pub(crate) struct Agent<P: Plugin> {
    host: Host,
    /// The person's setting: whether the server should run.
    wanted: bool,
    server: Server<Tools<P>>,
    running: Option<Running>,
    /// Why the server didn't start, if it didn't.
    error: Option<String>,
}

impl<P: Plugin> Agent<P> {
    pub(crate) fn new(host: Host) -> Self {
        // The protocol layer runs as if headless: the instance's token is
        // the person's approval, so a client that has it isn't asked about
        // again. Commands that ask first still ask, through the tools.
        let server = Server::new(
            Mode::Headless { autonomous: false },
            Tools::new(host.clone(), Mode::App, "the plug-in"),
        );
        Self {
            host,
            wanted: false,
            server,
            running: None,
            error: None,
        }
    }

    pub(crate) fn host(&self) -> &Host {
        &self.host
    }

    pub(crate) fn wanted(&self) -> bool {
        self.wanted
    }

    /// Switches the server on or off, now. Switching it on again tries
    /// again after a failed start.
    pub(crate) fn set_wanted(&mut self, on: bool) {
        self.wanted = on;
        if on && self.running.is_none() {
            self.error = None;
        }
        self.reconcile();
    }

    /// Starts or stops the server to match the setting.
    fn reconcile(&mut self) {
        match (self.wanted, self.running.is_some()) {
            (true, false) if self.error.is_none() => self.start(),
            (false, true) => self.stop(),
            (false, false) => self.error = None,
            _ => {}
        }
    }

    fn start(&mut self) {
        let started = http::new_token().and_then(|token| {
            let wake = self
                .host
                .waker()
                .unwrap_or_else(|| Arc::new(|| {}) as lspk_plugin::Waker);
            let endpoint = Endpoint {
                path: PATH.to_owned(),
                realm: P::INFO.name.to_owned(),
                token_location: "the plug-in's agent settings, or its entry in the discovery file"
                    .to_owned(),
            };
            let config = HttpConfig {
                port: 0,
                token: token.clone(),
            };
            HttpServer::start(&config, endpoint, wake).map(|http| (http, token))
        });
        let (http, token) = match started {
            Ok(started) => started,
            Err(error) => {
                // A sandboxed host may refuse a listening socket.
                self.error = Some(format!("the agent server couldn't start: {error}"));
                return;
            }
        };
        // Sessions from an earlier run used another token.
        let old: Vec<String> = self
            .server
            .sessions()
            .map(|(id, _)| id.to_owned())
            .collect();
        for id in old {
            self.server.close_session(&id);
        }
        let mut entry = Entry::new(
            P::INFO.id,
            P::INFO.name,
            &http.url(),
            http.local_addr().port(),
            &token,
        );
        entry.host = self.host.host_name();
        entry.track = self.host.track_name();
        let (registration, discovery) = match Discovery::user() {
            Some(discovery) => match discovery.register(entry) {
                Ok(registration) => (
                    Some(registration),
                    Ok(discovery.file().display().to_string()),
                ),
                Err(error) => (
                    None,
                    Err(format!(
                        "couldn't write the discovery file in {}: {error}",
                        discovery.dir().display()
                    )),
                ),
            },
            None => (
                None,
                Err("there's no per-user folder for the discovery file".to_owned()),
            ),
        };
        self.error = None;
        self.running = Some(Running {
            http,
            token,
            parked: Vec::new(),
            pending: Vec::new(),
            registration,
            discovery,
        });
    }

    fn stop(&mut self) {
        if let Some(running) = self.running.take() {
            let Running {
                http,
                parked,
                pending,
                registration,
                ..
            } = running;
            // Their connections end when their replies can't come.
            drop((parked, pending));
            http.stop();
            drop(registration);
        }
        self.error = None;
    }

    /// Serves what's waiting: finished previews, then requests, then calls
    /// that can now go on. On the main thread, from `on_main_thread`.
    pub(crate) fn pump(&mut self, plugin: &mut P) {
        self.reconcile();
        let Some(running) = self.running.as_mut() else {
            return;
        };
        while let Some((ticket, result)) = self.server.program_mut().take_finished() {
            self.server.finish_deferred(ticket, |_| result);
        }
        while let Some(job) = running.http.try_job() {
            let served = http::serve(&mut self.server, plugin, job);
            keep(running, served);
        }
        while let Some(served) = http::resume(&mut self.server, plugin, &mut running.parked) {
            keep(running, served);
        }
    }

    /// The track's name may have changed: rewrites the discovery entry.
    pub(crate) fn track_changed(&mut self) {
        let track = self.host.track_name();
        if let Some(registration) = self.running.as_mut().and_then(|r| r.registration.as_mut()) {
            let _ = registration.update(|entry| entry.track = track);
        }
    }

    pub(crate) fn status(&self) -> ServerStatus {
        let running = self.running.as_ref();
        ServerStatus {
            on: self.wanted,
            running: running.is_some(),
            url: running.map(|r| r.http.url()),
            token: running.map(|r| r.token.clone()),
            discovery: running.and_then(|r| r.discovery.clone().ok()),
            error: self
                .error
                .clone()
                .or_else(|| running.and_then(|r| r.discovery.clone().err())),
        }
    }

    /// The questions waiting for the person, oldest first.
    pub(crate) fn questions(&self) -> Vec<Question> {
        self.running.as_ref().map_or_else(Vec::new, |r| {
            r.pending.iter().map(|p| p.question().clone()).collect()
        })
    }

    /// Answers question `id`; false if none has that ID.
    pub(crate) fn answer(&mut self, plugin: &mut P, id: u64, allow: bool) -> bool {
        let Some(running) = self.running.as_mut() else {
            return false;
        };
        let Some(index) = running.pending.iter().position(|p| p.question().id == id) else {
            return false;
        };
        running
            .pending
            .remove(index)
            .finish(&mut self.server, plugin, allow);
        true
    }
}

impl<P: Plugin> Drop for Agent<P> {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Keeps a request that waits.
fn keep(running: &mut Running, served: Served) {
    match served {
        Served::Done => {}
        Served::Waiting(pending) => running.pending.push(pending),
        Served::Parked(parked) => running.parked.push(parked),
    }
}
