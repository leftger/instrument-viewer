use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use crate::backend::Backend;
use crate::config::{apply_section, read_config, ConfigSection, InstrumentConfig};
use crate::scpi::ScpiSession;
use crate::waveform::ChannelTrace;

pub enum Cmd {
    Connect { addr: String },
    Disconnect,
    Fetch { channels: Vec<String> },
    FetchSequence { channels: Vec<String> },
    ReadConfig,
    ApplyConfig(ConfigSection),
    RawQuery(String),
    RawWrite(String),
    Autoset,
}

pub enum Msg {
    Status(String),
    Connected(String),
    Disconnected,
    Traces(Vec<ChannelTrace>),
    Config(Box<InstrumentConfig>),
    Applied(String),
    RawResponse(String),
    Error(String),
}

pub struct Worker {
    tx: Sender<Cmd>,
    rx: Receiver<Msg>,
}

/// A live session paired with the backend chosen from its `*IDN?`, so the two
/// can never disagree about which instrument is on the far end.
struct Connection {
    session: ScpiSession,
    backend: Box<dyn Backend>,
}

/// The socket server can stop answering after heavy reconnect churn while still
/// accepting TCP, so one silent `*IDN?` is not proof the instrument is gone.
/// Clear the queue and ask again before giving up.
fn identify(s: &mut ScpiSession) -> Result<String, String> {
    match s.query("*IDN?") {
        Ok(idn) => return Ok(idn),
        Err(e) => {
            if !matches!(e, crate::scpi::ScpiError::Timeout) {
                return Err(format!("IDN failed: {e}"));
            }
        }
    }

    std::thread::sleep(Duration::from_millis(300));
    let _ = s.resync();
    match s.query("*IDN?") {
        Ok(idn) => Ok(idn),
        Err(_) => Err(
            "no response to *IDN? (TCP is open). The socket server is wedged — \
             wait ~30 s and reconnect, or toggle Utility → I/O → Socket Server off/on."
                .to_string(),
        ),
    }
}

/// Drop a session only after its output queue is empty.
///
/// The instrument's queue outlives the TCP connection, so closing a socket with
/// an unread response pending both corrupts the next session's replies and,
/// repeated enough times, stops the socket server answering at all.
fn close_session(connection: &mut Option<Connection>) {
    if let Some(c) = connection.as_mut() {
        let _ = c.session.resync();
    }
    *connection = None;
}

fn fetch_traces(c: &mut Connection, channels: &[String]) -> Result<Vec<ChannelTrace>, String> {
    let mut traces = Vec::new();
    for ch in channels {
        traces.push(
            c.backend
                .fetch_channel(&mut c.session, ch)
                .map_err(|e| format!("{ch}: {e}"))?,
        );
    }
    Ok(traces)
}

fn handle_fetch(
    connection: &mut Option<Connection>,
    msg_tx: &Sender<Msg>,
    repaint: impl Fn(),
    channels: &[String],
    sequence: bool,
) {
    let Some(c) = connection.as_mut() else {
        let _ = msg_tx.send(Msg::Error("Not connected".into()));
        repaint();
        return;
    };
    if sequence {
        let _ = msg_tx.send(Msg::Status(
            "Single acquire: waiting for trigger/complete…".into(),
        ));
        repaint();
        if let Err(e) = c
            .backend
            .wait_sequence(&mut c.session, Duration::from_secs(30))
        {
            let _ = c.session.resync();
            let _ = msg_tx.send(Msg::Error(format!("Sequence acquire failed: {e}")));
            repaint();
            return;
        }
    }
    match fetch_traces(c, channels) {
        Ok(traces) => {
            let _ = msg_tx.send(Msg::Traces(traces));
        }
        Err(first_error) => {
            let _ = c.session.resync();
            thread::sleep(Duration::from_millis(150));
            match fetch_traces(c, channels) {
                Ok(traces) => {
                    let _ = msg_tx.send(Msg::Traces(traces));
                }
                Err(second_error) => {
                    let _ = c.session.resync();
                    let _ = msg_tx.send(Msg::Error(format!(
                        "{second_error} (retry after {first_error})"
                    )));
                }
            }
        }
    }
    repaint();
}

impl Worker {
    pub fn spawn(repaint: impl Fn() + Send + 'static) -> Self {
        let (cmd_tx, cmd_rx) = channel::<Cmd>();
        let (msg_tx, msg_rx) = channel::<Msg>();

        thread::spawn(move || {
            let mut connection: Option<Connection> = None;
            while let Ok(cmd) = cmd_rx.recv() {
                match cmd {
                    Cmd::Connect { addr } => {
                        // Close any previous socket first: reconnecting while the
                        // old one is still open leaves two live sessions.
                        close_session(&mut connection);
                        let _ = msg_tx.send(Msg::Status(format!("Connecting to {addr}…")));
                        repaint();

                        match ScpiSession::connect(&addr, Duration::from_secs(6)) {
                            Ok(mut s) => match identify(&mut s) {
                                Ok(idn) => {
                                    let backend = crate::backend::from_idn(&idn);
                                    match s.set_preamble(backend.preamble()) {
                                        Ok(()) => {
                                            let _ = msg_tx.send(Msg::Status(format!(
                                                "{} backend selected",
                                                backend.name()
                                            )));
                                            connection = Some(Connection {
                                                session: s,
                                                backend,
                                            });
                                            let _ = msg_tx.send(Msg::Connected(idn));
                                        }
                                        Err(e) => {
                                            let _ = msg_tx
                                                .send(Msg::Error(format!("Setup failed: {e}")));
                                        }
                                    }
                                }
                                Err(e) => {
                                    let _ = msg_tx.send(Msg::Error(e));
                                }
                            },
                            Err(e) => {
                                let hint = if addr.ends_with(":4000") {
                                    " (Rigol DHO900 scopes use port 5555)"
                                } else {
                                    ""
                                };
                                let _ =
                                    msg_tx.send(Msg::Error(format!("Connect failed: {e}{hint}")));
                            }
                        }
                        repaint();
                    }
                    Cmd::Disconnect => {
                        close_session(&mut connection);
                        let _ = msg_tx.send(Msg::Disconnected);
                        repaint();
                    }
                    Cmd::Fetch { channels } => {
                        handle_fetch(&mut connection, &msg_tx, &repaint, &channels, false);
                    }
                    Cmd::FetchSequence { channels } => {
                        handle_fetch(&mut connection, &msg_tx, &repaint, &channels, true);
                    }
                    Cmd::ReadConfig => {
                        let Some(c) = connection.as_mut() else {
                            let _ = msg_tx.send(Msg::Error("Not connected".into()));
                            repaint();
                            continue;
                        };
                        let _ = msg_tx.send(Msg::Status("Reading instrument settings…".into()));
                        repaint();
                        match read_config(&mut c.session, c.backend.as_ref()) {
                            Ok(config) => {
                                let _ = msg_tx.send(Msg::Config(Box::new(config)));
                            }
                            Err(e) => {
                                let _ = c.session.resync();
                                let _ = msg_tx
                                    .send(Msg::Error(format!("Reading settings failed: {e}")));
                            }
                        }
                        repaint();
                    }
                    Cmd::ApplyConfig(section) => {
                        let Some(c) = connection.as_mut() else {
                            let _ = msg_tx.send(Msg::Error("Not connected".into()));
                            repaint();
                            continue;
                        };
                        match apply_section(&mut c.session, c.backend.as_ref(), &section) {
                            Ok(()) => {
                                let _ = msg_tx.send(Msg::Applied("Settings applied".into()));
                            }
                            Err(e) => {
                                let _ = c.session.resync();
                                let _ = msg_tx
                                    .send(Msg::Error(format!("Applying settings failed: {e}")));
                            }
                        }
                        repaint();
                    }
                    Cmd::RawQuery(command) => {
                        let Some(c) = connection.as_mut() else {
                            let _ = msg_tx.send(Msg::Error("Not connected".into()));
                            repaint();
                            continue;
                        };
                        match c.session.query(&command) {
                            Ok(response) => {
                                let _ = msg_tx.send(Msg::RawResponse(response));
                            }
                            Err(e) => {
                                let _ = c.session.resync();
                                let _ = msg_tx.send(Msg::Error(format!("Query failed: {e}")));
                            }
                        }
                        repaint();
                    }
                    Cmd::RawWrite(command) => {
                        let Some(c) = connection.as_mut() else {
                            let _ = msg_tx.send(Msg::Error("Not connected".into()));
                            repaint();
                            continue;
                        };
                        let sent = c
                            .session
                            .write(&command)
                            .and_then(|_| c.session.query("*OPC?").map(|_| ()));
                        match sent {
                            Ok(()) => {
                                let _ = msg_tx.send(Msg::Applied(format!("Sent: {command}")));
                            }
                            Err(e) => {
                                let _ = c.session.resync();
                                let _ = msg_tx.send(Msg::Error(format!("Command failed: {e}")));
                            }
                        }
                        repaint();
                    }
                    Cmd::Autoset => {
                        let Some(c) = connection.as_mut() else {
                            let _ = msg_tx.send(Msg::Error("Not connected".into()));
                            repaint();
                            continue;
                        };
                        match c.backend.autoset(&mut c.session) {
                            Ok(()) => {
                                let _ = msg_tx.send(Msg::Applied("Autoset started".into()));
                            }
                            Err(e) => {
                                let _ = msg_tx.send(Msg::Error(format!("Autoset failed: {e}")));
                            }
                        }
                        repaint();
                    }
                }
            }
        });

        Self {
            tx: cmd_tx,
            rx: msg_rx,
        }
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    pub fn try_recv(&self) -> Option<Msg> {
        match self.rx.try_recv() {
            Ok(m) => Some(m),
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => None,
        }
    }
}
