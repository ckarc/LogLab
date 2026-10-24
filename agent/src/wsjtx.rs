// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Hearing WSJT-X log a QSO.
//!
//! WSJT-X (and JTDX, which speaks the same messages) sends small UDP
//! messages to one address, 127.0.0.1:2237 unless set otherwise in its
//! Reporting settings. Its published description of them (in WSJT-X's
//! `NetworkMessage.hpp`) is the only source used here; this reader is LabLog's
//! own (Principle XVI). The Agent reads two of them:
//!
//! - **Heartbeat**, so it can say when WSJT-X starts and stops talking;
//! - **Logged ADIF**, sent when the operator clicks Log QSO: the QSO as ADIF.
//!
//! The messages carry no password, so the Agent listens only on this
//! computer and ignores anything from elsewhere (Principle XXI). Every
//! message is untrusted: lengths are checked against what arrived, and a
//! malformed one is dropped, never trusted (Principle XII).

use std::fmt;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

/// Every WSJT-X message starts with this number.
const MAGIC: u32 = 0xADBC_CBDA;
/// The message schemas this reader understands.
const SCHEMAS: [u32; 2] = [2, 3];
/// A UDP datagram is never bigger than this.
const MAX_DATAGRAM: usize = 65_536;
/// The `QDataStream` length that means "no string".
const NULL_LENGTH: u32 = 0xFFFF_FFFF;
/// How long one wait for a message lasts, so the listener can be stopped.
const WAIT: Duration = Duration::from_millis(500);

/// The message numbers this reader uses.
const HEARTBEAT: u32 = 0;
const CLOSE: u32 = 6;
const LOGGED_ADIF: u32 = 12;

/// A message from WSJT-X that the Agent acts on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    /// WSJT-X is running. `id` names the WSJT-X (several can run at once).
    Heartbeat {
        /// Which WSJT-X.
        id: String,
        /// Its version, as it reports it.
        version: String,
    },
    /// WSJT-X is closing.
    Close {
        /// Which WSJT-X.
        id: String,
    },
    /// The operator logged a QSO; `adif` is it, as WSJT-X wrote it.
    LoggedAdif {
        /// Which WSJT-X.
        id: String,
        /// The QSO as ADIF, with WSJT-X's little header.
        adif: String,
    },
    /// Any other message, which the Agent does not need.
    Other,
}

/// Why a datagram was not a WSJT-X message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Malformed(pub &'static str);

impl fmt::Display for Malformed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "not a WSJT-X message: {}", self.0)
    }
}

/// Reads `QDataStream` values from a datagram, never past its end.
struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Malformed> {
        if n > self.data.len() {
            return Err(Malformed("it ends too soon"));
        }
        let (head, rest) = self.data.split_at(n);
        self.data = rest;
        Ok(head)
    }

    fn u32(&mut self) -> Result<u32, Malformed> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// A UTF-8 string: its length, then its bytes. "No string" reads as empty.
    fn text(&mut self) -> Result<String, Malformed> {
        let length = self.u32()?;
        if length == NULL_LENGTH {
            return Ok(String::new());
        }
        let length = usize::try_from(length).map_err(|_| Malformed("a string is too long"))?;
        let bytes = self.take(length)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| Malformed("a string is not UTF-8"))
    }
}

/// Read one datagram.
///
/// # Errors
///
/// It is not a WSJT-X message, uses a schema this reader doesn't know, or
/// is cut short.
pub fn parse(data: &[u8]) -> Result<Message, Malformed> {
    if data.len() > MAX_DATAGRAM {
        return Err(Malformed("it is too big"));
    }
    let mut reader = Reader { data };
    if reader.u32()? != MAGIC {
        return Err(Malformed("it does not start with WSJT-X's number"));
    }
    if !SCHEMAS.contains(&reader.u32()?) {
        return Err(Malformed("its schema is one this Agent doesn't know"));
    }
    let kind = reader.u32()?;
    let id = reader.text()?;
    Ok(match kind {
        HEARTBEAT => {
            let _max_schema = reader.u32()?;
            Message::Heartbeat {
                id,
                version: reader.text()?,
            }
        }
        CLOSE => Message::Close { id },
        LOGGED_ADIF => Message::LoggedAdif {
            id,
            adif: reader.text()?,
        },
        _ => Message::Other,
    })
}

/// Check that an address is on this computer only.
///
/// # Errors
///
/// It is not a loopback address.
pub fn check_local(address: SocketAddr) -> Result<(), String> {
    if address.ip().is_loopback() {
        Ok(())
    } else {
        Err(format!(
            "--wsjtx must be on this computer, like 127.0.0.1:2237, not {address}: WSJT-X's messages carry no password (Principle XXI)"
        ))
    }
}

/// A socket WSJT-X sends its messages to.
pub struct Listener {
    socket: UdpSocket,
    buffer: Vec<u8>,
}

impl Listener {
    /// Listen at `address`, which must be on this computer.
    ///
    /// # Errors
    ///
    /// The address is not local, or another program is already using it.
    pub fn bind(address: SocketAddr) -> Result<Self, String> {
        check_local(address)?;
        let socket = UdpSocket::bind(address).map_err(|error| {
            format!(
                "cannot listen for WSJT-X at {address}: {error} (is another program, such as GridTracker or JTAlert, using it?)"
            )
        })?;
        socket
            .set_read_timeout(Some(WAIT))
            .map_err(|error| format!("cannot listen for WSJT-X: {error}"))?;
        Ok(Self {
            socket,
            buffer: vec![0; MAX_DATAGRAM],
        })
    }

    /// The next message, or `None` after a short wait with nothing heard.
    /// Datagrams from other computers, and ones that are not WSJT-X
    /// messages, are dropped.
    pub fn next(&mut self) -> Option<Result<Message, Malformed>> {
        match self.socket.recv_from(&mut self.buffer) {
            Ok((n, from)) if from.ip().is_loopback() => Some(parse(&self.buffer[..n])),
            Ok(_) => Some(Err(Malformed("it came from another computer"))),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                None
            }
            Err(_) => Some(Err(Malformed("it could not be received"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CLOSE, HEARTBEAT, LOGGED_ADIF, MAGIC, Malformed, Message, check_local, parse};

    fn text(out: &mut Vec<u8>, value: &str) {
        let length = u32::try_from(value.len()).unwrap_or(u32::MAX);
        out.extend_from_slice(&length.to_be_bytes());
        out.extend_from_slice(value.as_bytes());
    }

    fn message(kind: u32, id: &str) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&MAGIC.to_be_bytes());
        out.extend_from_slice(&2u32.to_be_bytes());
        out.extend_from_slice(&kind.to_be_bytes());
        text(&mut out, id);
        out
    }

    const ADIF: &str =
        "\n<adif_ver:5>3.1.0\n<programid:6>WSJT-X\n<EOH>\n<call:4>W1AW <mode:3>FT8 <EOR>";

    #[test]
    fn reads_a_logged_qso() {
        let mut data = message(LOGGED_ADIF, "WSJT-X");
        text(&mut data, ADIF);
        assert_eq!(
            parse(&data),
            Ok(Message::LoggedAdif {
                id: "WSJT-X".to_owned(),
                adif: ADIF.to_owned()
            })
        );
    }

    #[test]
    fn reads_a_heartbeat_and_a_close() {
        let mut data = message(HEARTBEAT, "WSJT-X");
        data.extend_from_slice(&3u32.to_be_bytes());
        text(&mut data, "2.7.0");
        text(&mut data, "abc123");
        assert_eq!(
            parse(&data),
            Ok(Message::Heartbeat {
                id: "WSJT-X".to_owned(),
                version: "2.7.0".to_owned()
            })
        );
        assert_eq!(
            parse(&message(CLOSE, "JTDX")),
            Ok(Message::Close {
                id: "JTDX".to_owned()
            })
        );
    }

    #[test]
    fn other_messages_are_passed_over() {
        // A Decode (2) or a Status (1) is a message, just not one the Agent uses.
        assert_eq!(parse(&message(2, "WSJT-X")), Ok(Message::Other));
    }

    #[test]
    fn refuses_what_is_not_a_wsjtx_message() {
        assert!(parse(b"").is_err());
        assert!(parse(b"hello world, not wsjt-x").is_err());
        let mut wrong_schema = message(LOGGED_ADIF, "WSJT-X");
        wrong_schema[4..8].copy_from_slice(&9u32.to_be_bytes());
        assert!(parse(&wrong_schema).is_err());
    }

    #[test]
    fn never_reads_past_the_end() {
        // A string that claims to be longer than the datagram.
        let mut data = message(LOGGED_ADIF, "WSJT-X");
        data.extend_from_slice(&1000u32.to_be_bytes());
        data.extend_from_slice(b"<call:4>W1AW");
        assert_eq!(parse(&data), Err(Malformed("it ends too soon")));
        // Every cut-short prefix of a good message is refused, not trusted.
        let mut whole = message(LOGGED_ADIF, "WSJT-X");
        text(&mut whole, ADIF);
        for n in 0..whole.len() {
            assert!(parse(&whole[..n]).is_err(), "{n} bytes should be refused");
        }
    }

    #[test]
    fn refuses_a_string_that_is_not_utf8() {
        let mut data = message(LOGGED_ADIF, "WSJT-X");
        data.extend_from_slice(&2u32.to_be_bytes());
        data.extend_from_slice(&[0xFF, 0xFE]);
        assert_eq!(parse(&data), Err(Malformed("a string is not UTF-8")));
    }

    #[test]
    fn listens_only_on_this_computer() {
        assert!(check_local("127.0.0.1:2237".parse().unwrap_or_else(|_| unreachable!())).is_ok());
        assert!(check_local("[::1]:2237".parse().unwrap_or_else(|_| unreachable!())).is_ok());
        assert!(check_local("0.0.0.0:2237".parse().unwrap_or_else(|_| unreachable!())).is_err());
        assert!(
            check_local(
                "192.168.1.5:2237"
                    .parse()
                    .unwrap_or_else(|_| unreachable!())
            )
            .is_err()
        );
    }
}
