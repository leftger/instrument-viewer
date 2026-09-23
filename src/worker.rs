use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use crate::config::{apply_section, read_config, ConfigSection, InstrumentConfig};
use crate::scpi::ScpiSession;
use crate::waveform::{fetch_channel, ChannelTrace};

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
fn close_session(session: &mut Option<ScpiSession>) {
    if let Some(s) = session.as_mut() {
        let _ = s.resync();
    }
    *session = None;
}

fn fetch_traces(s: &mut ScpiSession, channels: &[String]) -> Result<Vec<ChannelTrace>, String> {
    let mut traces = Vec::new();
    for ch in channels {
        traces.push(fetch_channel(s, ch).map_err(|e| format!("{ch}: {e}"))?);
    }
    Ok(traces)
}

fn handle_fetch(
    session: &mut Option<ScpiSession>,
    msg_tx: &Sender<Msg>,
    repaint: impl Fn(),
    channels: &[String],
    sequence: bool,
) {
    let Some(s) = session.as_mut() else {
        let _ = msg_tx.send(Msg::Error("Not connected".into()));
        repaint();
        return;
    };
    if sequence {
        let _ = msg_tx.send(Msg::Status(
            "SEQUENCE acquire: waiting for trigger/complete…".into(),
        ));
        repaint();
        if let Err(e) = crate::acquire::wait_sequence(s, Duration::from_secs(30)) {
            let _ = s.resync();
            let _ = msg_tx.send(Msg::Error(format!("Sequence acquire failed: {e}")));
            repaint();
            return;
        }
    }
    match fetch_traces(s, channels) {
        Ok(traces) => {
            let _ = msg_tx.send(Msg::Traces(traces));
        }
        Err(first_error) => {
            let _ = s.resync();
            thread::sleep(Duration::from_millis(150));
            match fetch_traces(s, channels) {
                Ok(traces) => {
                    let _ = msg_tx.send(Msg::Traces(traces));
                }
                Err(second_error) => {
                    let _ = s.resync();
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
            let mut session: Option<ScpiSession> = None;
            while let Ok(cmd) = cmd_rx.recv() {
                match cmd {
                    Cmd::Connect { addr } => {
                        // Close any previous socket first: reconnecting while the
                        // old one is still open leaves two live sessions.
                        close_session(&mut session);
                        let _ = msg_tx.send(Msg::Status(format!("Connecting to {addr}…")));
                        repaint();

                        match ScpiSession::connect(&addr, Duration::from_secs(6)) {
                            Ok(mut s) => match identify(&mut s) {
                                Ok(idn) => {
                                    session = Some(s);
                                    let _ = msg_tx.send(Msg::Connected(idn));
                                }
                                Err(e) => {
                                    let _ = msg_tx.send(Msg::Error(e));
                                }
                            },
                            Err(e) => {
                                let _ = msg_tx.send(Msg::Error(format!("Connect failed: {e}")));
                            }
                        }
                        repaint();
                    }
                    Cmd::Disconnect => {
                        close_session(&mut session);
                        let _ = msg_tx.send(Msg::Disconnected);
                        repaint();
                    }
                    Cmd::Fetch { channels } => {
                        handle_fetch(&mut session, &msg_tx, &repaint, &channels, false);
                    }
                    Cmd::FetchSequence { channels } => {
                        handle_fetch(&mut session, &msg_tx, &repaint, &channels, true);
                    }
                    Cmd::ReadConfig => {
                        let Some(s) = session.as_mut() else {
                            let _ = msg_tx.send(Msg::Error("Not connected".into()));
                            repaint();
                            continue;
                        };
                        let _ = msg_tx.send(Msg::Status("Reading instrument settings…".into()));
                        repaint();
                        match read_config(s) {
                            Ok(config) => {
                                let _ = msg_tx.send(Msg::Config(Box::new(config)));
                            }
                            Err(e) => {
                                let _ = s.resync();
                                let _ = msg_tx
                                    .send(Msg::Error(format!("Reading settings failed: {e}")));
                            }
                        }
                        repaint();
                    }
                    Cmd::ApplyConfig(section) => {
                        let Some(s) = session.as_mut() else {
                            let _ = msg_tx.send(Msg::Error("Not connected".into()));
                            repaint();
                            continue;
                        };
                        match apply_section(s, &section) {
                            Ok(()) => {
                                let _ = msg_tx.send(Msg::Applied("Settings applied".into()));
                            }
                            Err(e) => {
                                let _ = s.resync();
                                let _ = msg_tx
                                    .send(Msg::Error(format!("Applying settings failed: {e}")));
                            }
                        }
                        repaint();
                    }
                    Cmd::RawQuery(command) => {
                        let Some(s) = session.as_mut() else {
                            let _ = msg_tx.send(Msg::Error("Not connected".into()));
                            repaint();
                            continue;
                        };
                        match s.query(&command) {
                            Ok(response) => {
                                let _ = msg_tx.send(Msg::RawResponse(response));
                            }
                            Err(e) => {
                                let _ = s.resync();
                                let _ = msg_tx.send(Msg::Error(format!("Query failed: {e}")));
                            }
                        }
                        repaint();
                    }
                    Cmd::RawWrite(command) => {
                        let Some(s) = session.as_mut() else {
                            let _ = msg_tx.send(Msg::Error("Not connected".into()));
                            repaint();
                            continue;
                        };
                        match s.write(&command).and_then(|_| s.query("*OPC?").map(|_| ())) {
                            Ok(()) => {
                                let _ = msg_tx.send(Msg::Applied(format!("Sent: {command}")));
                            }
                            Err(e) => {
                                let _ = s.resync();
                                let _ = msg_tx.send(Msg::Error(format!("Command failed: {e}")));
                            }
                        }
                        repaint();
                    }
                    Cmd::Autoset => {
                        let Some(s) = session.as_mut() else {
                            let _ = msg_tx.send(Msg::Error("Not connected".into()));
                            repaint();
                            continue;
                        };
                        match s.write("AUTOSET EXECUTE") {
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
