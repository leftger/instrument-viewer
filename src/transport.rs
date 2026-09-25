//! Byte-level transports for SCPI sessions.
//!
//! `ScpiSession` speaks SCPI; a [`Transport`] only moves bytes. TCP talks to a
//! LAN socket server (Tektronix: protocol None, port 4000; Rigol DHO900: 5555;
//! Keysight/Siglent: 5025). USB uses USBTMC bulk transfers on class `0xFE` /
//! subclass `0x03` (no NI-VISA).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::scpi::{trace, ScpiError};
use crate::usbtmc::UsbtmcDevice;

pub trait Transport: Send {
    fn write_message(&mut self, data: &[u8]) -> Result<(), ScpiError>;
    fn read_line(&mut self) -> Result<String, ScpiError>;
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), ScpiError>;
    fn drain(&mut self);
}

/// Plain TCP socket server transport.
pub struct TcpTransport {
    writer: TcpStream,
    reader: BufReader<TcpStream>,
}

impl TcpTransport {
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
        Ok(Self {
            writer: stream.try_clone()?,
            reader: BufReader::new(stream),
        })
    }
}

impl Transport for TcpTransport {
    fn write_message(&mut self, data: &[u8]) -> Result<(), ScpiError> {
        self.writer.write_all(data)?;
        self.writer.flush()?;
        Ok(())
    }

    fn read_line(&mut self) -> Result<String, ScpiError> {
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => Err(ScpiError::Empty),
            Ok(_) => Ok(line.trim().to_string()),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                Err(ScpiError::Timeout)
            }
            Err(e) => Err(e.into()),
        }
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
        let buffered = self.reader.buffer().len();
        self.reader.consume(buffered);
        dropped += buffered;
        if dropped > 0 {
            trace(|| format!("!! drained {dropped} stale bytes"));
        }
        let _ = self.writer.set_nonblocking(false);
    }
}

/// USBTMC bulk-transfer transport.
pub struct UsbtmcTransport(UsbtmcDevice);

impl Transport for UsbtmcTransport {
    fn write_message(&mut self, data: &[u8]) -> Result<(), ScpiError> {
        self.0.write_message(data)
    }

    fn read_line(&mut self) -> Result<String, ScpiError> {
        self.0.read_line()
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), ScpiError> {
        self.0.read_exact(buf)
    }

    fn drain(&mut self) {
        self.0.drain();
    }
}

/// Pick the transport for an address: `usb:...` opens USBTMC, anything else TCP.
pub fn connect(addr: &str, timeout: Duration) -> Result<Box<dyn Transport>, ScpiError> {
    if crate::usbtmc::is_usb_addr(addr) {
        Ok(Box::new(UsbtmcTransport(UsbtmcDevice::open(
            addr, timeout,
        )?)))
    } else {
        Ok(Box::new(TcpTransport::connect(addr, timeout)?))
    }
}
