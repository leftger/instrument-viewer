use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ScpiError {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("empty SCPI response")]
    Empty,
    #[error("invalid IEEE 488.2 definite-length block")]
    BadBlock,
    #[error("timeout waiting for instrument response")]
    Timeout,
    #[error("bad address {0}")]
    Addr(String),
    #[error("invalid response: {0}")]
    Parse(String),
    #[error("{0}")]
    Unsupported(String),
}

impl ScpiError {
    pub fn is_connection_refused(&self) -> bool {
        matches!(self, Self::Io(error) if error.kind() == std::io::ErrorKind::ConnectionRefused)
    }
}

/// Log the SCPI exchange to stderr when `MDO_TRACE` is set.
fn trace(msg: impl FnOnce() -> String) {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    if *ON.get_or_init(|| std::env::var_os("MDO_TRACE").is_some()) {
        eprintln!("[scpi] {}", msg());
    }
}

/// Raw TCP SCPI session against the scope's socket server (Tektronix: protocol
/// None, port 4000; Rigol DHO900: port 5555).
///
/// The session is long-lived on purpose. The instrument keeps a single output
/// queue that survives a TCP disconnect, so abandoning an unread response
/// leaves it to be delivered to the *next* connection, shifting every later
/// reply one query behind.
pub struct ScpiSession {
    writer: TcpStream,
    reader: BufReader<TcpStream>,
    line_buf: Vec<u8>,
    preamble: Vec<String>,
}

impl ScpiSession {
    pub fn connect(addr: &str, timeout: Duration) -> Result<Self, ScpiError> {
        let sock: SocketAddr = addr
            .to_socket_addrs()
            .map_err(|_| ScpiError::Addr(addr.to_string()))?
            .next()
            .ok_or_else(|| ScpiError::Addr(addr.to_string()))?;

        let stream = TcpStream::connect_timeout(&sock, timeout)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        stream.set_nodelay(true)?;

        let mut session = Self {
            writer: stream.try_clone()?,
            reader: BufReader::new(stream),
            line_buf: Vec::with_capacity(64),
            preamble: Vec::new(),
        };
        session.resync()?;
        Ok(session)
    }

    /// Commands re-sent on every `resync`, supplied by the backend once the
    /// instrument has identified itself.
    ///
    /// Connecting has to come before identifying, so this cannot be known when
    /// the session is built. A Rigol answers the Tektronix setup commands with
    /// `-100,"Command err"`, which is harmless but pointless.
    pub fn set_preamble(&mut self, commands: Vec<String>) -> Result<(), ScpiError> {
        self.preamble = commands;
        self.resync()
    }

    /// Clear the instrument's status and discard anything still sitting in its
    /// output queue from a previous session.
    pub fn resync(&mut self) -> Result<(), ScpiError> {
        self.drain();
        self.write("*CLS")?;
        for cmd in self.preamble.clone() {
            self.write(&cmd)?;
        }
        std::thread::sleep(Duration::from_millis(50));
        self.drain();
        Ok(())
    }

    fn drain(&mut self) {
        let _ = self.writer.set_nonblocking(true);
        let mut scratch = [0u8; 4096];
        let mut dropped = 0usize;
        loop {
            match self.reader.get_mut().read(&mut scratch) {
                Ok(0) => break,
                Ok(n) => {
                    dropped += n;
                    continue;
                }
                Err(_) => break,
            }
        }
        // Buffered bytes belong to the discarded exchange too.
        let buffered = self.reader.buffer().len();
        self.reader.consume(buffered);
        dropped += buffered;
        if dropped > 0 {
            trace(|| format!("!! drained {dropped} stale bytes"));
        }
        let _ = self.writer.set_nonblocking(false);
    }

    /// Send one command, terminator included, as a single write.
    ///
    /// The terminator must go out in the same segment as the command. With
    /// `TCP_NODELAY` set, writing it separately puts a bare `\n` in its own
    /// packet, and the socket server's parser stops responding after a few
    /// dozen such commands.
    pub fn write(&mut self, cmd: &str) -> Result<(), ScpiError> {
        trace(|| format!("-> {cmd}"));
        self.line_buf.clear();
        self.line_buf.extend_from_slice(cmd.as_bytes());
        self.line_buf.push(b'\n');
        self.writer.write_all(&self.line_buf)?;
        self.writer.flush()?;
        Ok(())
    }

    pub fn query(&mut self, cmd: &str) -> Result<String, ScpiError> {
        self.write(cmd)?;
        // A leftover LF from a previous block or drain can yield an empty
        // line; skip those so the next query does not eat this command's reply.
        loop {
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) => return Err(ScpiError::Empty),
                Ok(_) => {
                    let line = line.trim().to_string();
                    if line.is_empty() {
                        continue;
                    }
                    trace(|| format!("<- {line}"));
                    return Ok(line);
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    return Err(ScpiError::Timeout);
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Read an IEEE 488.2 definite-length block: `#Nddd...<data>` then a trailing LF.
    pub fn query_binary_block(&mut self, cmd: &str) -> Result<Vec<u8>, ScpiError> {
        self.write(cmd)?;

        // Skip any leading whitespace or header text before the block marker.
        let mut b = [0u8; 1];
        let mut guard = 0;
        loop {
            self.read_exact(&mut b)?;
            if b[0] == b'#' {
                break;
            }
            guard += 1;
            if guard > 256 {
                return Err(ScpiError::BadBlock);
            }
        }

        self.read_exact(&mut b)?;
        if !b[0].is_ascii_digit() {
            return Err(ScpiError::BadBlock);
        }
        let ndigits = (b[0] - b'0') as usize;
        if ndigits == 0 {
            return Err(ScpiError::BadBlock);
        }

        let mut len_buf = vec![0u8; ndigits];
        self.read_exact(&mut len_buf)?;
        let nbytes: usize = std::str::from_utf8(&len_buf)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .ok_or(ScpiError::BadBlock)?;

        let mut payload = vec![0u8; nbytes];
        self.read_exact(&mut payload)?;

        // Trailing LF terminates the block.
        let _ = self.read_exact(&mut b);
        trace(|| format!("<- <{nbytes} byte block>, term {:?}", b[0] as char));
        Ok(payload)
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), ScpiError> {
        match self.reader.read_exact(buf) {
            Ok(()) => Ok(()),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                Err(ScpiError::Timeout)
            }
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_only_connection_refused_errors() {
        let refused = ScpiError::Io(std::io::Error::from(std::io::ErrorKind::ConnectionRefused));
        let timeout = ScpiError::Io(std::io::Error::from(std::io::ErrorKind::TimedOut));
        assert!(refused.is_connection_refused());
        assert!(!timeout.is_connection_refused());
        assert!(!ScpiError::Timeout.is_connection_refused());
    }
}
