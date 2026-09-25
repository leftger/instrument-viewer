use std::convert::TryFrom;
use std::time::Duration;

use scpi::parser::tokenizer::{Token, Tokenizer};
use thiserror::Error;

use crate::transport::Transport;

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

/// One IEEE 488.2 program-message unit, as split by the `scpi` tokenizer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramUnit {
    pub query: bool,
    pub text: String,
}

/// Split a program message on `;` using IEEE 488.2 header/string/block rules.
pub fn parse_program(input: &str) -> Result<Vec<ProgramUnit>, ScpiError> {
    let bytes = input.as_bytes();
    let mut tokenizer = Tokenizer::new(bytes);
    let mut units = Vec::new();
    let mut unit_start = 0usize;
    let mut query = false;
    let mut saw_header = false;

    let pos = |t: &Tokenizer<'_>| bytes.len() - t.chars.as_slice().len();

    loop {
        let before = pos(&tokenizer);
        let Some(token) = tokenizer.next() else {
            break;
        };
        let token = token.map_err(|e| ScpiError::Parse(format!("{e:?}: {input:?}")))?;
        match token {
            Token::ProgramMnemonic(_) => saw_header = true,
            Token::HeaderQuerySuffix => query = true,
            Token::ProgramMessageUnitSeparator => {
                push_unit(bytes, unit_start, before, query, saw_header, &mut units)?;
                unit_start = pos(&tokenizer);
                query = false;
                saw_header = false;
            }
            _ => {}
        }
    }
    push_unit(
        bytes,
        unit_start,
        bytes.len(),
        query,
        saw_header,
        &mut units,
    )?;
    if units.is_empty() {
        return Err(ScpiError::Parse(format!("empty SCPI program {input:?}")));
    }
    Ok(units)
}

fn push_unit(
    bytes: &[u8],
    start: usize,
    end: usize,
    query: bool,
    saw_header: bool,
    units: &mut Vec<ProgramUnit>,
) -> Result<(), ScpiError> {
    let text = std::str::from_utf8(&bytes[start..end]).unwrap_or("").trim();
    if text.is_empty() {
        return Ok(());
    }
    if !saw_header {
        return Err(ScpiError::Parse(format!(
            "SCPI unit has no header: {text:?}"
        )));
    }
    units.push(ProgramUnit {
        query,
        text: text.to_string(),
    });
    Ok(())
}

fn first_data_token(text: &str) -> Result<Token<'_>, ScpiError> {
    let bytes = text.trim().as_bytes();
    for token in Tokenizer::new_params(bytes) {
        let token = token.map_err(|e| ScpiError::Parse(format!("{e:?}: {text:?}")))?;
        if token.is_data() {
            return Ok(token);
        }
    }
    Err(ScpiError::Parse(format!("no SCPI data in {text:?}")))
}

fn token_error(token: Token<'_>, text: &str) -> ScpiError {
    ScpiError::Parse(format!("invalid SCPI data {text:?} ({token:?})"))
}

/// Parse a decimal or quoted numeric response (`1`, `1.0000E+04`, `"0.5"`).
pub fn parse_f64(text: &str) -> Result<f64, ScpiError> {
    match first_data_token(text)? {
        Token::StringProgramData(inner) => {
            let inner = std::str::from_utf8(inner).unwrap_or("");
            parse_f64(inner)
        }
        token => f64::try_from(token).map_err(|_| token_error(token, text)),
    }
}

/// Parse a count, accepting both integer and scientific-notation replies.
pub fn parse_count(text: &str) -> Option<u64> {
    let token = first_data_token(text).ok()?;
    if let Ok(value) = u64::try_from(token) {
        return Some(value);
    }
    let value = parse_f64(text).ok()?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    Some(value.round() as u64)
}

/// Boolean program data: `0`/`1`, `ON`/`OFF`, plus instrument `RUN`/`STOP`.
pub fn parse_bool(text: &str) -> Result<bool, ScpiError> {
    let token = first_data_token(text)?;
    match bool::try_from(token) {
        Ok(value) => Ok(value),
        Err(_) => match token {
            Token::CharacterProgramData(s)
                if s.eq_ignore_ascii_case(b"RUN") || s.eq_ignore_ascii_case(b"1") =>
            {
                Ok(true)
            }
            Token::CharacterProgramData(s)
                if s.eq_ignore_ascii_case(b"STOP") || s.eq_ignore_ascii_case(b"0") =>
            {
                Ok(false)
            }
            _ => Err(token_error(token, text)),
        },
    }
}

/// Character or quoted string data, uppercased. Leaves numeric replies as digits.
pub fn parse_character(text: &str) -> String {
    let raw = match first_data_token(text) {
        Ok(
            Token::CharacterProgramData(s)
            | Token::StringProgramData(s)
            | Token::DecimalNumericProgramData(s),
        ) => String::from_utf8_lossy(s).into_owned(),
        _ => text.trim().trim_matches('"').to_string(),
    };
    raw.to_ascii_uppercase()
}

/// CLI SI value (`500mV`, `2us`, `20MHz`). Uses the SCPI numeric+suffix tokenizer.
pub fn parse_si_value(input: &str) -> Result<f64, String> {
    let ascii = input.replace('µ', "u");
    let token = first_data_token(&ascii).map_err(|e| e.to_string())?;
    match token {
        Token::DecimalNumericProgramData(_) => {
            f64::try_from(token).map_err(|_| format!("invalid SI value {input:?}"))
        }
        Token::DecimalNumericSuffixProgramData(num, suffix) => {
            let value = f64::try_from(Token::DecimalNumericProgramData(num))
                .map_err(|_| format!("invalid SI value {input:?}"))?;
            let factor =
                si_suffix_factor(suffix).ok_or_else(|| format!("invalid SI value {input:?}"))?;
            Ok(value * factor)
        }
        _ => Err(format!("invalid SI value {input:?}")),
    }
}

fn si_suffix_factor(suffix: &[u8]) -> Option<f64> {
    let suffixes = [
        (b"GHz" as &[u8], 1e9),
        (b"MHz", 1e6),
        (b"kHz", 1e3),
        (b"Hz", 1.0),
        (b"mV", 1e-3),
        (b"uV", 1e-6),
        (b"V", 1.0),
        (b"ms", 1e-3),
        (b"us", 1e-6),
        (b"ns", 1e-9),
        (b"ps", 1e-12),
        (b"s", 1.0),
    ];
    suffixes
        .iter()
        .find(|(name, _)| *name == suffix)
        .map(|(_, factor)| *factor)
}

/// CLI record length (`10000`, `10k`, `5M`).
pub fn parse_record_length(input: &str) -> Result<u64, String> {
    let token = first_data_token(input).map_err(|e| e.to_string())?;
    match token {
        Token::DecimalNumericProgramData(_) => {
            u64::try_from(token).map_err(|_| format!("invalid record length {input:?}"))
        }
        Token::DecimalNumericSuffixProgramData(num, suffix) => {
            let value = u64::try_from(Token::DecimalNumericProgramData(num))
                .map_err(|_| format!("invalid record length {input:?}"))?;
            let factor = match suffix {
                b"k" | b"K" => 1_000,
                b"m" | b"M" => 1_000_000,
                _ => return Err(format!("invalid record length {input:?}")),
            };
            value
                .checked_mul(factor)
                .ok_or_else(|| format!("invalid record length {input:?}"))
        }
        _ => Err(format!("invalid record length {input:?}")),
    }
}

/// Log the SCPI exchange to stderr when `MDO_TRACE` is set.
pub(crate) fn trace(msg: impl FnOnce() -> String) {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    if *ON.get_or_init(|| std::env::var_os("MDO_TRACE").is_some()) {
        eprintln!("[scpi] {}", msg());
    }
}

/// SCPI session over a byte-level [`Transport`] (TCP or USBTMC).
///
/// TCP talks to the instrument socket server (Tektronix: protocol None, port
/// 4000; Rigol DHO900: 5555; Keysight/Siglent: 5025). USB uses USBTMC bulk
/// transfers on class `0xFE` / subclass `0x03`.
///
/// The session is long-lived on purpose. The instrument keeps a single output
/// queue that survives a TCP disconnect, so abandoning an unread response
/// leaves it to be delivered to the *next* connection, shifting every later
/// reply one query behind.
pub struct ScpiSession {
    transport: Box<dyn Transport>,
    line_buf: Vec<u8>,
    preamble: Vec<String>,
}

impl ScpiSession {
    pub fn connect(addr: &str, timeout: Duration) -> Result<Self, ScpiError> {
        let transport = crate::transport::connect(addr, timeout)?;
        let mut session = Self {
            transport,
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
        self.transport.drain();
    }

    /// Reject syntactically invalid program messages before they hit the socket.
    pub fn validate_program(cmd: &str) -> Result<(), ScpiError> {
        parse_program(cmd).map(|_| ())
    }

    /// Send one command, terminator included, as a single write.
    ///
    /// The terminator must go out in the same segment as the command. With
    /// `TCP_NODELAY` set, writing it separately puts a bare `\n` in its own
    /// packet, and the socket server's parser stops responding after a few
    /// dozen such commands.
    pub fn write(&mut self, cmd: &str) -> Result<(), ScpiError> {
        Self::validate_program(cmd)?;
        self.write_raw(cmd)
    }

    fn write_raw(&mut self, cmd: &str) -> Result<(), ScpiError> {
        trace(|| format!("-> {cmd}"));
        self.line_buf.clear();
        self.line_buf.extend_from_slice(cmd.as_bytes());
        self.line_buf.push(b'\n');
        self.transport.write_message(&self.line_buf)
    }

    pub fn query(&mut self, cmd: &str) -> Result<String, ScpiError> {
        Self::validate_program(cmd)?;
        self.write_raw(cmd)?;
        // A leftover LF from a previous block or drain can yield an empty
        // line; skip those so the next query does not eat this command's reply.
        loop {
            let line = self.transport.read_line()?;
            if line.is_empty() {
                continue;
            }
            trace(|| format!("<- {line}"));
            return Ok(line);
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
        self.transport.read_exact(buf)
    }

    /// Read a block whose payload length is an ASCII field *inside* a
    /// fixed-size binary header instead of the standard `#N<digits>` position.
    ///
    /// The Hantek HRDO2000 answers `:WAVeform:DATA:DISP?` with `#9`, nine
    /// reserved bytes, the nine-digit byte count, more reserved bytes, and then
    /// one byte per sample. `header_len` is the header size (128 there),
    /// `len_start`/`len_len` locate the count field within it.
    ///
    /// Returns the header followed by the payload, so the caller can read the
    /// scaling fields out of it.
    pub fn query_header_block(
        &mut self,
        cmd: &str,
        header_len: usize,
        len_start: usize,
        len_len: usize,
    ) -> Result<Vec<u8>, ScpiError> {
        /// Refuse an implausible count rather than trying to allocate it.
        const MAX_BLOCK_BYTES: usize = 64 * 1024 * 1024;

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

        // The marker is the header's first byte; the rest is binary.
        let mut header = vec![0u8; header_len];
        if header_len == 0 {
            return Err(ScpiError::BadBlock);
        }
        header[0] = b'#';
        self.read_exact(&mut header[1..])?;

        let nbytes = parse_header_len(&header, len_start, len_len).ok_or(ScpiError::BadBlock)?;
        if nbytes > MAX_BLOCK_BYTES {
            return Err(ScpiError::BadBlock);
        }

        let mut payload = vec![0u8; nbytes];
        self.read_exact(&mut payload)?;
        // Trailing LF terminates the block.
        let _ = self.read_exact(&mut b);
        trace(|| format!("<- <{nbytes} byte payload in {header_len} byte header>"));
        header.extend_from_slice(&payload);
        Ok(header)
    }
}

/// Read the ASCII byte count out of a binary block header.
fn parse_header_len(header: &[u8], len_start: usize, len_len: usize) -> Option<usize> {
    let end = len_start.checked_add(len_len)?;
    let field = header.get(len_start..end)?;
    let text = std::str::from_utf8(field).ok()?;
    text.trim().trim_start_matches('+').parse().ok()
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

    #[test]
    fn reads_length_from_a_binary_header() {
        // Mirrors the HRDO2000 header: '#9', 9 reserved bytes, 9 ASCII digits.
        let mut header = vec![b'#', b'9'];
        header.extend_from_slice(b"123456789"); // reserved
        header.extend_from_slice(b"000001024"); // payload byte count
        header.extend_from_slice(&[0u8; 100]); // more reserved
        assert_eq!(parse_header_len(&header, 11, 9), Some(1024));
        // A space-padded field still parses.
        let mut padded = header.clone();
        padded[11..20].copy_from_slice(b"     1024");
        assert_eq!(parse_header_len(&padded, 11, 9), Some(1024));
    }

    #[test]
    fn rejects_out_of_range_header_field() {
        let header = vec![0u8; 20];
        assert_eq!(parse_header_len(&header, 11, 9), None);
        assert_eq!(parse_header_len(&header, 18, 9), None);
    }

    #[test]
    fn splits_compound_program_messages() {
        let units = parse_program("*CLS; CH1:SCALE 0.5; *IDN?").unwrap();
        assert_eq!(
            units,
            vec![
                ProgramUnit {
                    query: false,
                    text: "*CLS".into()
                },
                ProgramUnit {
                    query: false,
                    text: "CH1:SCALE 0.5".into()
                },
                ProgramUnit {
                    query: true,
                    text: "*IDN?".into()
                },
            ]
        );
    }

    #[test]
    fn parses_known_instrument_commands() {
        for cmd in [
            "*CLS",
            "*IDN?",
            "*OPC?",
            "HEADER OFF",
            "VERBOSE OFF",
            "HORIZONTAL:RECORDLENGTH?",
            "HORIZONTAL:SCALE?",
            "WFMOutpre?",
            "CURVE?",
            "DATA:ENC RIBINARY",
            "DATA:SOURCE CH1",
            "TRIGGER:A:LEVEL:CH1 0.5",
            "ACQUIRE:STOPAFTER SEQUENCE",
            "AUTOSET EXECUTE",
            ":CHAN1:DISP ON",
            ":WAV:FORM WORD",
            ":SING",
        ] {
            parse_program(cmd).unwrap_or_else(|e| panic!("{cmd}: {e}"));
        }
    }

    #[test]
    fn rejects_garbage_program() {
        assert!(parse_program("not a command!!!").is_err());
        assert!(parse_program("").is_err());
    }

    #[test]
    fn parses_numeric_and_bool_responses() {
        assert_eq!(parse_f64("1.0000E+04").unwrap(), 10_000.0);
        assert_eq!(parse_f64("\"0.5\"").unwrap(), 0.5);
        assert_eq!(parse_count("10000"), Some(10_000));
        assert_eq!(parse_count("1.0000E+04"), Some(10_000));
        assert_eq!(parse_count(" 1.0000E+03 "), Some(1_000));
        assert_eq!(parse_count("MEG"), None);
        assert_eq!(parse_count(""), None);
        assert_eq!(parse_count("-1"), None);
        assert_eq!(parse_count("inf"), None);
        assert!(parse_bool("ON").unwrap());
        assert!(!parse_bool("OFF").unwrap());
        assert!(parse_bool("1").unwrap());
        assert!(parse_bool("RUN").unwrap());
        assert!(!parse_bool("STOP").unwrap());
        assert_eq!(parse_character("\"dc\""), "DC");
        assert_eq!(parse_si_value("500mV").unwrap(), 0.5);
        assert_eq!(parse_si_value("2us").unwrap(), 2e-6);
        assert_eq!(parse_si_value("20MHz").unwrap(), 20e6);
        assert_eq!(parse_record_length("10k").unwrap(), 10_000);
        assert_eq!(parse_record_length("5M").unwrap(), 5_000_000);
    }
}
