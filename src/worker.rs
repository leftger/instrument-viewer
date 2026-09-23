use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use crate::backend::{AcquisitionStatus, Backend, InstrumentCapabilities};
use crate::config::{
    apply_section, include_current_values, read_config, validate_section, ConfigSection,
    InstrumentConfig,
};
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
    PollStatus,
}

pub enum Msg {
    Status(String),
    Connected {
        idn: String,
        port: u16,
        capabilities: Box<InstrumentCapabilities>,
    },
    Disconnected,
    Traces(Vec<ChannelTrace>),
    Config {
        config: Box<InstrumentConfig>,
        capabilities: Box<InstrumentCapabilities>,
    },
    Applied(String),
    AcquisitionStatus(Option<AcquisitionStatus>),
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
    capabilities: InstrumentCapabilities,
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

fn split_endpoint(addr: &str) -> Option<(&str, u16)> {
    let (host, port) = addr.rsplit_once(':')?;
    Some((host, port.parse().ok()?))
}

fn port_candidates(preferred: u16) -> Vec<u16> {
    let mut ports = vec![preferred];
    for standard in [4000, 5555] {
        if !ports.contains(&standard) {
            ports.push(standard);
        }
    }
    ports
}

fn connect_with_fallback(
    addr: &str,
    msg_tx: &Sender<Msg>,
    repaint: &impl Fn(),
) -> Result<(ScpiSession, u16), String> {
    let (host, preferred) =
        split_endpoint(addr).ok_or_else(|| format!("Connect failed: bad address {addr}"))?;
    let ports = port_candidates(preferred);
    let mut refused = Vec::new();

    for (index, port) in ports.iter().copied().enumerate() {
        let endpoint = format!("{host}:{port}");
        let _ = msg_tx.send(Msg::Status(format!("Connecting to {endpoint}…")));
        repaint();
        match ScpiSession::connect(&endpoint, Duration::from_secs(6)) {
            Ok(session) => return Ok((session, port)),
            Err(error) if error.is_connection_refused() => {
                refused.push(port);
                if index + 1 < ports.len() {
                    let _ = msg_tx.send(Msg::Status(format!(
                        "Port {port} refused; trying {}…",
                        ports[index + 1]
                    )));
                    repaint();
                }
            }
            Err(error) => return Err(format!("Connect failed: {error}")),
        }
    }

    Err(format!(
        "Connect failed: connection refused on ports {}",
        refused
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    ))
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
                        match connect_with_fallback(&addr, &msg_tx, &repaint) {
                            Ok((mut s, port)) => match identify(&mut s) {
                                Ok(idn) => {
                                    let backend = crate::backend::from_idn(&idn);
                                    let capabilities = backend.capabilities();
                                    match s.set_preamble(backend.preamble()) {
                                        Ok(()) => {
                                            let _ = msg_tx.send(Msg::Status(format!(
                                                "{} backend selected",
                                                backend.name()
                                            )));
                                            connection = Some(Connection {
                                                session: s,
                                                backend,
                                                capabilities: capabilities.clone(),
                                            });
                                            let _ = msg_tx.send(Msg::Connected {
                                                idn,
                                                port,
                                                capabilities: Box::new(capabilities),
                                            });
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
                            Err(error) => {
                                let _ = msg_tx.send(Msg::Error(error));
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
                                include_current_values(&mut c.capabilities, &config);
                                let _ = msg_tx.send(Msg::Config {
                                    config: Box::new(config),
                                    capabilities: Box::new(c.capabilities.clone()),
                                });
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
                        let applied = validate_section(&section, &c.capabilities).and_then(|_| {
                            apply_section(&mut c.session, c.backend.as_ref(), &section)
                        });
                        match applied {
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
                    Cmd::PollStatus => {
                        // This is a best-effort heartbeat, not a user operation.
                        // In particular, do not resync or reconnect on failure:
                        // extra recovery traffic keeps a wedged Tek parser sick.
                        let status = match connection.as_mut() {
                            Some(c) => c.backend.acquisition_status(&mut c.session).ok(),
                            None => None,
                        };
                        let _ = msg_tx.send(Msg::AcquisitionStatus(status));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_port_candidates_prefer_requested_port() {
        assert_eq!(port_candidates(4000), vec![4000, 5555]);
        assert_eq!(port_candidates(5555), vec![5555, 4000]);
        assert_eq!(port_candidates(1234), vec![1234, 4000, 5555]);
    }

    #[test]
    fn splits_ipv4_and_bracketed_ipv6_endpoints() {
        assert_eq!(
            split_endpoint("169.254.1.2:5555"),
            Some(("169.254.1.2", 5555))
        );
        assert_eq!(split_endpoint("[::1]:4000"), Some(("[::1]", 4000)));
        assert_eq!(split_endpoint("missing-port"), None);
    }
}
