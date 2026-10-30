// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Reading one retained message from an MQTT broker (MQTT 3.1.1, from its
//! OASIS standard), the way the Engine reads FieldLab's bus
//! (`engine/src/loglab_engine/mqtt.py`).
//!
//! The Agent only reads: it connects, subscribes to one topic, takes the
//! retained message the broker hands every new subscriber, and
//! disconnects. It never publishes. Everything the broker sends is
//! untrusted (Principle XII): every packet is size-capped, and the payload
//! is returned as bytes for the caller to check. The broker is optional
//! (Principle V): every failure is an error the caller treats as "not known
//! this time".

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

/// How long a read may take, all told.
pub const TIMEOUT: Duration = Duration::from_secs(3);
/// The largest packet the Agent takes.
const MAX_PACKET: usize = 256 * 1024;
const KEEPALIVE: u16 = 30;
const CONNECT: u8 = 0x10;
const CONNACK: u8 = 0x20;
const PUBLISH: u8 = 0x30;
/// SUBSCRIBE, with the flags the standard requires.
const SUBSCRIBE: u8 = 0x82;
const SUBACK: u8 = 0x90;
const DISCONNECT: [u8; 2] = [0xe0, 0x00];
/// How long to wait after the subscription is acknowledged for a retained
/// message; nothing by then means nothing is retained.
const RETAINED_WAIT: Duration = Duration::from_secs(1);
const PACKET_ID: u16 = 1;

/// `text` as the standard writes a string: its length, then its bytes.
fn string(text: &str) -> Vec<u8> {
    let data = text.as_bytes();
    let mut out = u16::try_from(data.len())
        .unwrap_or(u16::MAX)
        .to_be_bytes()
        .to_vec();
    out.extend_from_slice(&data[..data.len().min(usize::from(u16::MAX))]);
    out
}

/// The standard's variable-length "remaining length".
fn length(mut size: usize) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let digit = u8::try_from(size % 128).unwrap_or(0);
        size /= 128;
        out.push(digit | if size > 0 { 0x80 } else { 0 });
        if size == 0 {
            return out;
        }
    }
}

fn packet(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![kind];
    out.extend(length(body.len()));
    out.extend_from_slice(body);
    out
}

/// Why a packet couldn't be read.
enum Failed {
    /// The deadline passed.
    Late,
    /// Anything else.
    Error(String),
}

struct Reader {
    stream: TcpStream,
    deadline: Instant,
    buffer: Vec<u8>,
}

impl Reader {
    fn shorten(&mut self, wait: Duration) {
        self.deadline = self.deadline.min(Instant::now() + wait);
    }

    fn take(&mut self, size: usize) -> Result<Vec<u8>, Failed> {
        while self.buffer.len() < size {
            let left = self.deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Failed::Late);
            }
            let _ = self.stream.set_read_timeout(Some(left));
            let mut chunk = [0_u8; 4096];
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    return Err(Failed::Error("the broker closed the connection".to_owned()));
                }
                Ok(read) => self.buffer.extend_from_slice(&chunk[..read]),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return Err(Failed::Late);
                }
                Err(error) => return Err(Failed::Error(error.to_string())),
            }
        }
        Ok(self.buffer.drain(..size).collect())
    }

    fn packet(&mut self) -> Result<(u8, Vec<u8>), Failed> {
        let kind = self.take(1)?[0];
        let mut size = 0_usize;
        let mut shift = 0;
        let mut ended = false;
        for _ in 0..4 {
            let digit = self.take(1)?[0];
            size |= usize::from(digit & 0x7f) << shift;
            shift += 7;
            if digit & 0x80 == 0 {
                ended = true;
                break;
            }
        }
        if !ended {
            return Err(Failed::Error("a packet's length is malformed".to_owned()));
        }
        if size > MAX_PACKET {
            return Err(Failed::Error(format!(
                "a packet is larger than {MAX_PACKET} bytes"
            )));
        }
        Ok((kind, self.take(size)?))
    }
}

/// A PUBLISH packet's payload, if it is the message retained on `topic`.
fn matching(kind: u8, body: &[u8], topic: &str) -> Result<Option<Vec<u8>>, Failed> {
    let malformed = || Failed::Error("a message is malformed".to_owned());
    let size = usize::from(u16::from_be_bytes([
        *body.first().ok_or_else(malformed)?,
        *body.get(1).ok_or_else(malformed)?,
    ]));
    let name = body.get(2..2 + size).ok_or_else(malformed)?;
    // A packet id comes with QoS 1 and 2.
    let rest = 2 + size + if kind & 0x06 != 0 { 2 } else { 0 };
    let payload = body.get(rest..).ok_or_else(malformed)?;
    // 0x01: the retain flag.
    Ok((name == topic.as_bytes() && kind & 0x01 != 0).then(|| payload.to_vec()))
}

/// The retained message the broker sends a new subscriber, if any. The
/// broker sends it with the subscription's acknowledgement; once that has
/// come, a short wait with nothing more means nothing is retained.
fn retained(reader: &mut Reader, topic: &str) -> Result<Option<Vec<u8>>, Failed> {
    let mut acknowledged = false;
    loop {
        let (kind, body) = match reader.packet() {
            Ok(packet) => packet,
            Err(Failed::Late) if acknowledged => return Ok(None),
            Err(failed) => return Err(failed),
        };
        if kind == SUBACK && !acknowledged {
            if body.get(2) == Some(&0x80) {
                return Err(Failed::Error(format!(
                    "the broker refused the subscription to {topic}"
                )));
            }
            acknowledged = true;
            reader.shorten(RETAINED_WAIT);
        } else if kind & 0xf0 == PUBLISH
            && let Some(found) = matching(kind, &body, topic)?
        {
            return Ok(Some(found));
        }
    }
}

/// The message retained on `topic` (no wildcards) at the broker
/// `host`:`port`, or `None` if there is none.
///
/// # Errors
///
/// The broker couldn't be reached in [`TIMEOUT`], refused the connection,
/// or answered with something the standard doesn't allow.
pub fn read_retained(
    host: &str,
    port: u16,
    topic: &str,
    client_id: &str,
) -> Result<Option<Vec<u8>>, String> {
    let address = (host, port)
        .to_socket_addrs()
        .map_err(|error| format!("the broker {host}:{port} could not be found ({error})"))?
        .next()
        .ok_or_else(|| format!("the broker {host}:{port} could not be found"))?;
    let mut stream = TcpStream::connect_timeout(&address, TIMEOUT)
        .map_err(|error| format!("the broker could not be reached ({error})"))?;
    let _ = stream.set_write_timeout(Some(TIMEOUT));
    let mut connect = string("MQTT");
    // Protocol level 4 (3.1.1); a clean session.
    connect.extend([4, 0x02]);
    connect.extend(KEEPALIVE.to_be_bytes());
    connect.extend(string(client_id));
    let mut subscribe = PACKET_ID.to_be_bytes().to_vec();
    subscribe.extend(string(topic));
    // QoS 0.
    subscribe.push(0);
    let sent = |stream: &mut TcpStream, bytes: &[u8]| {
        stream
            .write_all(bytes)
            .map_err(|error| format!("the broker could not be reached ({error})"))
    };
    sent(&mut stream, &packet(CONNECT, &connect))?;
    let mut reader = Reader {
        stream: stream
            .try_clone()
            .map_err(|error| format!("the broker could not be reached ({error})"))?,
        deadline: Instant::now() + TIMEOUT,
        buffer: Vec::new(),
    };
    let said = |failed| match failed {
        Failed::Late => "the broker did not answer in time".to_owned(),
        Failed::Error(why) => why,
    };
    let (kind, body) = reader.packet().map_err(said)?;
    if kind != CONNACK || body.len() != 2 {
        return Err("the broker did not acknowledge the connection".to_owned());
    }
    if body[1] != 0 {
        return Err(format!(
            "the broker refused the connection (code {})",
            body[1]
        ));
    }
    sent(&mut stream, &packet(SUBSCRIBE, &subscribe))?;
    let found = retained(&mut reader, topic).map_err(said)?;
    let _ = stream.write_all(&DISCONNECT);
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::{CONNACK, PUBLISH, SUBACK, length, packet, read_retained, string};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn writes_lengths_and_strings_as_the_standard_does() {
        // The standard's own examples: 0, 127, 128, 16383, 16384.
        assert_eq!(length(0), [0x00]);
        assert_eq!(length(127), [0x7f]);
        assert_eq!(length(128), [0x80, 0x01]);
        assert_eq!(length(16_383), [0xff, 0x7f]);
        assert_eq!(length(16_384), [0x80, 0x80, 0x01]);
        assert_eq!(string("MQTT"), b"\x00\x04MQTT");
    }

    /// A broker that answers one client with `answers` after its CONNECT
    /// and SUBSCRIBE, and hands back what the client sent.
    fn broker(answers: Vec<Vec<u8>>) -> (u16, thread::JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| unreachable!("{e}"));
        let port = listener
            .local_addr()
            .unwrap_or_else(|e| unreachable!("{e}"))
            .port();
        let serving = thread::spawn(move || {
            let (mut client, _) = listener.accept().unwrap_or_else(|e| unreachable!("{e}"));
            let mut heard = Vec::new();
            let mut chunk = [0_u8; 1024];
            // CONNECT, then the acknowledgement.
            let read = client.read(&mut chunk).unwrap_or(0);
            heard.extend_from_slice(&chunk[..read]);
            let _ = client.write_all(&packet(CONNACK, &[0, 0]));
            // SUBSCRIBE, then what this broker says.
            let read = client.read(&mut chunk).unwrap_or(0);
            heard.extend_from_slice(&chunk[..read]);
            for answer in answers {
                let _ = client.write_all(&answer);
            }
            let read = client.read(&mut chunk).unwrap_or(0);
            heard.extend_from_slice(&chunk[..read]);
            heard
        });
        (port, serving)
    }

    fn publish(topic: &str, payload: &[u8], retained: bool) -> Vec<u8> {
        let mut body = string(topic);
        body.extend_from_slice(payload);
        packet(PUBLISH | u8::from(retained), &body)
    }

    #[test]
    fn reads_the_retained_message() {
        let fix = br#"{"lat":41.5,"lon":-93.6,"ts":1}"#;
        let (port, serving) = broker(vec![
            packet(SUBACK, &[0, 1, 0]),
            publish("fieldlab/other", b"no", true),
            publish("fieldlab/location", fix, true),
        ]);
        let found = read_retained("127.0.0.1", port, "fieldlab/location", "test");
        assert_eq!(found, Ok(Some(fix.to_vec())));
        let heard = serving.join().unwrap_or_default();
        // CONNECT for MQTT 3.1.1, SUBSCRIBE for the topic, then DISCONNECT.
        assert!(heard.starts_with(b"\x10"));
        assert!(heard.windows(6).any(|w| w == b"MQTT\x04\x02"));
        assert!(heard.windows(19).any(|w| w == b"\x00\x11fieldlab/location"));
        assert!(heard.ends_with(&[0xe0, 0x00]));
    }

    #[test]
    fn nothing_retained_is_none_and_a_live_message_doesnt_count() {
        let (port, serving) = broker(vec![
            packet(SUBACK, &[0, 1, 0]),
            publish("fieldlab/location", b"live", false),
        ]);
        let found = read_retained("127.0.0.1", port, "fieldlab/location", "test");
        assert_eq!(found, Ok(None));
        let _ = serving.join();
    }

    #[test]
    fn a_refused_subscription_or_no_broker_is_an_error() {
        let (port, serving) = broker(vec![packet(SUBACK, &[0, 1, 0x80])]);
        assert!(read_retained("127.0.0.1", port, "fieldlab/location", "test").is_err());
        let _ = serving.join();
        // Nothing listens on a port just let go of.
        let free = TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .map_or_else(|e| unreachable!("{e}"), |address| address.port());
        assert!(read_retained("127.0.0.1", free, "fieldlab/location", "test").is_err());
    }
}
