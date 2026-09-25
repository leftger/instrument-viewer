//! A tiny fake SCPI instrument for tests.
//!
//! Real drivers are mostly session-bound: they write commands and parse the
//! replies. This module serves canned replies over a loopback TCP socket so
//! those paths can be exercised without hardware. Matching is a
//! case-insensitive substring test, first rule wins; unmatched writes are
//! ignored and unmatched queries get a marker reply so a missing rule shows up
//! as a parse error naming the command rather than a hang.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// One canned answer.
pub enum Reply {
    /// A line reply; the terminator is appended.
    Line(String),
    /// A standard `#N<digits>` definite-length block, terminator appended.
    Block(Vec<u8>),
    /// Exact bytes, terminator appended. For instruments whose header does not
    /// follow the `#N` convention (Hantek HRDO2000).
    Raw(Vec<u8>),
}

pub struct Rule {
    needle: String,
    reply: Reply,
}

impl Rule {
    /// Reply with `text` to any command containing `needle`.
    pub fn line(needle: &str, text: &str) -> Self {
        Self {
            needle: needle.to_ascii_uppercase(),
            reply: Reply::Line(text.to_string()),
        }
    }

    /// Reply with a definite-length block of `payload`.
    pub fn block(needle: &str, payload: Vec<u8>) -> Self {
        Self {
            needle: needle.to_ascii_uppercase(),
            reply: Reply::Block(payload),
        }
    }

    /// Reply with exact bytes (header included).
    pub fn raw(needle: &str, bytes: Vec<u8>) -> Self {
        Self {
            needle: needle.to_ascii_uppercase(),
            reply: Reply::Raw(bytes),
        }
    }
}

/// A running fake instrument.
pub struct MockInstrument {
    addr: String,
    stop: Arc<AtomicBool>,
    /// Every command the instrument received, in order.
    commands: Arc<Mutex<Vec<String>>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl MockInstrument {
    /// Bind a loopback port and serve `rules` on the first connection.
    pub fn start(rules: Vec<Rule>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock instrument");
        let addr = listener.local_addr().expect("mock address").to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let commands = Arc::new(Mutex::new(Vec::new()));
        let commands_thread = commands.clone();
        let handle = thread::spawn(move || {
            while !stop_thread.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => serve(stream, &rules, &stop_thread, &commands_thread),
                    Err(_) => break,
                }
            }
        });
        Self {
            addr,
            stop,
            commands,
            handle: Some(handle),
        }
    }

    /// Commands received so far, in order. Writes are recorded too, which is
    /// how the `apply_*` paths are asserted.
    pub fn commands(&self) -> Vec<String> {
        self.commands
            .lock()
            .map(|log| log.clone())
            .unwrap_or_default()
    }

    /// Did any command contain `needle` (case-insensitive)?
    pub fn saw(&self, needle: &str) -> bool {
        let upper = needle.to_ascii_uppercase();
        self.commands()
            .iter()
            .any(|cmd| cmd.to_ascii_uppercase().contains(&upper))
    }

    /// `host:port` to hand to `ScpiSession::connect`.
    pub fn addr(&self) -> &str {
        &self.addr
    }
}

impl Drop for MockInstrument {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Unblock `accept` if no client ever connected.
        let _ = TcpStream::connect(&self.addr);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn serve(stream: TcpStream, rules: &[Rule], stop: &AtomicBool, commands: &Mutex<Vec<String>>) {
    // A read timeout lets the loop notice `stop` even while a client keeps the
    // connection open.
    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    while !stop.load(Ordering::SeqCst) {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let command = line.trim();
                if command.is_empty() {
                    continue;
                }
                if let Ok(mut log) = commands.lock() {
                    log.push(command.to_string());
                }
                if !respond(&mut writer, rules, command) {
                    break;
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue
            }
            Err(_) => break,
        }
    }
}

/// Answer one command; returns false when the socket is gone.
fn respond(writer: &mut TcpStream, rules: &[Rule], command: &str) -> bool {
    let upper = command.to_ascii_uppercase();
    let reply = rules
        .iter()
        .find(|rule| contains_pattern(&upper, &rule.needle))
        .map(|rule| &rule.reply);
    match reply {
        Some(Reply::Line(text)) => send(writer, text.as_bytes()),
        Some(Reply::Block(payload)) => {
            let digits = payload.len().to_string().len();
            let mut out = format!("#{digits}{}", payload.len()).into_bytes();
            out.extend_from_slice(payload);
            send(writer, &out)
        }
        Some(Reply::Raw(bytes)) => send(writer, bytes),
        // `*OPC?` is a synchronization query every driver may use; answer it
        // even when the test did not think to add a rule.
        None if upper.starts_with("*OPC?") => send(writer, b"1"),
        // Only queries need an answer; unmatched writes are silently accepted.
        None if command.ends_with('?') => {
            eprintln!("mock: unmatched query {command:?}");
            send(writer, format!("UNMATCHED:{command}").as_bytes())
        }
        None => {
            eprintln!("mock: unmatched write {command:?}");
            true
        }
    }
}

fn send(writer: &mut TcpStream, bytes: &[u8]) -> bool {
    if writer.write_all(bytes).is_err() {
        return false;
    }
    writer.write_all(b"\n").is_ok() && writer.flush().is_ok()
}

/// Does `pattern` match anywhere in `text`? `*` matches any run of characters;
/// every other character, `?` included, is literal (SCPI queries end in `?`).
fn contains_pattern(text: &str, pattern: &str) -> bool {
    if !pattern.contains('*') {
        return text.contains(pattern);
    }
    (0..=text.len())
        .filter(|i| text.is_char_boundary(*i))
        .any(|i| glob_match(&text[i..], pattern))
}

/// Whole-string wildcard match with `*` as the only special character.
fn glob_match(text: &str, pattern: &str) -> bool {
    let t: Vec<char> = text.chars().collect();
    let p: Vec<char> = pattern.chars().collect();
    let (mut ti, mut pi) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut star_t = 0usize;
    while ti < t.len() {
        if pi < p.len() && p[pi] != '*' && p[pi] == t[ti] {
            ti += 1;
            pi += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            star_t = ti;
            pi += 1;
        } else if let Some(sp) = star {
            pi = sp + 1;
            star_t += 1;
            ti = star_t;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scpi::ScpiSession;

    #[test]
    fn serves_lines_blocks_and_answers_opc() {
        let mock = MockInstrument::start(vec![
            Rule::line("*IDN?", "FAKE,INSTRUMENT,SN,1.0"),
            Rule::line("MEASURE?", "1.25e-3"),
            Rule::block("TRACE?", vec![1, 2, 3, 4]),
        ]);
        let mut session = ScpiSession::connect(mock.addr(), Duration::from_secs(2)).unwrap();
        assert_eq!(session.query("*IDN?").unwrap(), "FAKE,INSTRUMENT,SN,1.0");
        assert_eq!(session.query("MEASURE?").unwrap(), "1.25e-3");
        assert_eq!(
            session.query_binary_block("TRACE?").unwrap(),
            vec![1, 2, 3, 4]
        );
        assert_eq!(session.query("*OPC?").unwrap(), "1");
    }

    #[test]
    fn unmatched_queries_name_the_command() {
        let mock = MockInstrument::start(vec![Rule::line("*IDN?", "FAKE")]);
        let mut session = ScpiSession::connect(mock.addr(), Duration::from_secs(2)).unwrap();
        let reply = session.query("CH1:SCALE?").unwrap();
        assert!(reply.contains("CH1:SCALE?"), "{reply}");
    }

    #[test]
    fn wildcards_stand_for_channel_numbers() {
        assert!(contains_pattern(":CHAN2:SCAL?", ":CHAN*:SCAL?"));
        assert!(contains_pattern("SELECT:CH4?", "SELECT:CH*?"));
        assert!(contains_pattern("CH1:SCALE?", "CH*:SCALE?"));
        assert!(contains_pattern("CH1:SCALE?", "*:SCALE?"));
        // A `?` in a pattern is literal, and `*` cannot invent characters.
        assert!(!contains_pattern("CH1:OFFSET?", "CH*:SCALE?"));
        assert!(!contains_pattern("HORIZONTAL:SCALE?", "CH*:SCALE?"));
        assert!(!glob_match("CH1:SCALE", "CH1:OFFSET?"));
        assert!(glob_match("", "*"));
        assert!(glob_match("ABC", "A*C"));
    }
}
