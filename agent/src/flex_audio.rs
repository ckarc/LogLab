// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! A FlexRadio's audio, carried by the Agent itself, with no DAX program.
//!
//! A FlexRadio sends a slice's audio over the network to any client that
//! asks (`stream create type=dax_rx dax_channel=N`), and takes transmit
//! audio from the client that says it supplies it (`dax audio set N tx=1`,
//! then `stream create type=dax_tx`): the API wiki's `stream` and `dax`
//! pages. The Agent opens its own API connection for this, beside the one
//! that tunes and keys the radio, so the audio has its own UDP port
//! (`client udpport`) and its own threads.
//!
//! The audio is VITA-49 packets, sent to and from UDP port 4991 on the
//! radio. With `client set send_reduced_bw_dax=true`, each packet carries
//! 128 mono samples at 24 kHz, 16-bit big-endian, with packet class
//! 0x534C0123; without it, 256 float samples at 48 kHz with class
//! 0x534C03E3. Those figures are not on the API wiki; they come from nDAX
//! (MIT, <https://github.com/kc2g-flex-tools/nDAX>, `main.go`), an
//! open-source DAX replacement, read for the facts and not copied
//! (Principle XVI). The Agent uses the reduced form: a quarter of the
//! network traffic, and 24 kHz is plenty for any digital mode's 3 kHz.
//!
//! Receive audio goes into a [`Buffer`] that the Agent plays into
//! VB-CABLE, as it does a USB radio's. Transmit audio, from the transmit
//! cable, goes to the radio only while the Agent has it keyed for a
//! program (`on_air`); the rest of the time nothing is sent and what the
//! program plays is thrown away, so nothing builds up to play late
//! (Principle IX).
//!
//! Every packet is checked before it is believed (Principle XII): it must
//! come from the radio, carry the receive stream's number and the audio
//! class, and be the size it says.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::audio::{Buffer, Resampler};

/// The radio's audio rate, in the reduced form the Agent asks for.
pub const RATE: u32 = 24_000;
/// Samples in one audio packet, in the reduced form.
pub const PACKET_SAMPLES: usize = 128;
/// The packet class code of reduced-bandwidth DAX audio.
const AUDIO_CLASS: u16 = 0x0123;
/// The class ID words of an audio packet: FlexRadio's OUI, then its
/// information class and the packet class.
const CLASS_ID: [u32; 2] = [0x0000_1C2D, 0x534C_0123];
/// The radio's UDP port for VITA-49 streams.
const VITA_PORT: u16 = 4991;
/// The DAX channel the Agent uses.
pub const CHANNEL: u8 = 1;
/// How long connecting, and each answer, may take.
const ANSWER_WAIT: Duration = Duration::from_secs(3);
/// How often the Agent asks the radio whether it is still there.
const PING_EVERY: Duration = Duration::from_secs(5);
/// The longest line read from the radio.
const MAX_LINE: usize = 8192;

/// The samples of one audio packet for `stream`, from -1.0 to 1.0, or
/// `None` for any other packet.
#[must_use]
pub fn samples(packet: &[u8], stream: u32) -> Option<Vec<f32>> {
    let id = packet.get(4..8)?;
    if u32::from_be_bytes(<[u8; 4]>::try_from(id).ok()?) != stream {
        return None;
    }
    let payload = crate::flex::vita_payload(packet, AUDIO_CLASS)?;
    if payload.len() % 2 != 0 || payload.len() > PACKET_SAMPLES * 2 * 4 {
        return None;
    }
    Some(
        payload
            .chunks_exact(2)
            .map(|pair| f32::from(i16::from_be_bytes([pair[0], pair[1]])) / 32768.0)
            .collect(),
    )
}

/// One audio packet for the radio: `samples` (at most
/// [`PACKET_SAMPLES`]) on `stream`, numbered `count` (only its low four
/// bits are sent). Laid out as nDAX's: IF data with a stream ID, class ID
/// present, timestamps "other" and "sample count", all zero.
#[must_use]
pub fn packet(stream: u32, count: u8, samples: &[f32]) -> Vec<u8> {
    let samples = &samples[..samples.len().min(PACKET_SAMPLES)];
    let words = 7 + samples.len() / 2;
    let header =
        0x18D0_0000 | (u32::from(count & 0x0F) << 16) | u32::try_from(words).unwrap_or(0) & 0xFFFF;
    let mut packet: Vec<u8> = [header, stream, CLASS_ID[0], CLASS_ID[1], 0, 0, 0]
        .iter()
        .flat_map(|word| word.to_be_bytes())
        .collect();
    for sample in samples {
        packet.extend_from_slice(&to_i16(*sample).to_be_bytes());
    }
    packet
}

/// A sample from -1.0 to 1.0 as a 16-bit one, clipped.
#[allow(clippy::cast_possible_truncation)]
fn to_i16(sample: f32) -> i16 {
    // Clamped first, so the cast loses nothing.
    (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

/// A stream number as the radio answers `stream create` with it: hex,
/// with or without `0x`.
#[must_use]
pub fn stream_id(answer: &str) -> Option<u32> {
    let hex = answer.trim();
    let hex = hex
        .strip_prefix("0x")
        .or_else(|| hex.strip_prefix("0X"))
        .unwrap_or(hex);
    u32::from_str_radix(hex, 16).ok().filter(|id| *id != 0)
}

/// The commands that set the radio's audio up for the Agent, in order,
/// for `slice`, with the Agent's UDP port `port`.
#[must_use]
pub fn set_up_commands(slice: u8, port: u16) -> Vec<String> {
    vec![
        format!("client udpport {port}"),
        "client set send_reduced_bw_dax=true".to_owned(),
        format!("slice set {slice} dax={CHANNEL}"),
        format!("dax audio set {CHANNEL} slice={slice} tx=1"),
    ]
}

/// The Agent's audio connection to a FlexRadio, until dropped.
pub struct FlexAudio {
    running: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Drop for FlexAudio {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// The radio's API connection for audio: numbered commands and their
/// answers.
struct Control {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    seq: u32,
    /// A line begun but not yet ended.
    pending: String,
}

impl Control {
    fn open(radio: SocketAddr) -> Result<Self, String> {
        let stream = TcpStream::connect_timeout(&radio, ANSWER_WAIT)
            .map_err(|error| format!("can't reach the FlexRadio at {radio} for audio: {error}"))?;
        stream
            .set_read_timeout(Some(Duration::from_millis(100)))
            .map_err(|error| error.to_string())?;
        let writer = stream.try_clone().map_err(|error| error.to_string())?;
        Ok(Self {
            reader: BufReader::new(stream),
            writer,
            seq: 0,
            pending: String::new(),
        })
    }

    /// Sends `command` and waits for its answer: the answer's message, or
    /// why the radio refused it.
    fn ask(&mut self, command: &str) -> Result<String, String> {
        self.seq += 1;
        let seq = self.seq;
        writeln!(self.writer, "C{seq}|{command}")
            .and_then(|()| self.writer.flush())
            .map_err(|error| format!("lost the FlexRadio's audio connection: {error}"))?;
        let deadline = Instant::now() + ANSWER_WAIT;
        while Instant::now() < deadline {
            let Some(line) = self.line()? else { continue };
            if let Some(answer) = answer_to(&line, seq) {
                return answer.map_err(|code| {
                    format!("the FlexRadio refused {command:?} (code {code:08X})")
                });
            }
        }
        Err(format!("the FlexRadio did not answer {command:?}"))
    }

    /// The next line from the radio, if one came in time. A line cut off
    /// by the wait is kept and finished on the next call.
    fn line(&mut self) -> Result<Option<String>, String> {
        let room = MAX_LINE.saturating_sub(self.pending.len()).max(1);
        let read = (&mut self.reader)
            .take(u64::try_from(room).unwrap_or(u64::MAX))
            .read_line(&mut self.pending);
        match read {
            Ok(0) => Err("the FlexRadio closed its audio connection".to_owned()),
            Ok(_) if self.pending.ends_with('\n') || self.pending.len() >= MAX_LINE => {
                let line = self.pending.trim_end().to_owned();
                self.pending.clear();
                Ok(Some(line))
            }
            Ok(_) => Ok(None),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(format!("lost the FlexRadio's audio connection: {error}")),
        }
    }
}

/// The answer to command `seq` in `line` (`R<seq>|<hex code>|<message>`):
/// its message, or the code it was refused with. `None` for any other line.
fn answer_to(line: &str, seq: u32) -> Option<Result<String, u32>> {
    let rest = line.strip_prefix('R')?;
    let mut parts = rest.splitn(3, '|');
    if parts.next()?.parse::<u32>().ok()? != seq {
        return None;
    }
    let code = u32::from_str_radix(parts.next()?.trim(), 16).ok()?;
    let message = parts.next().unwrap_or_default().to_owned();
    Some(if code == 0 { Ok(message) } else { Err(code) })
}

impl FlexAudio {
    /// Starts the FlexRadio at `radio`'s audio for `slice`: its receive
    /// audio into `receive` (at [`RATE`]), and, while `on_air`, `transmit`
    /// to it.
    ///
    /// # Errors
    ///
    /// The radio can't be reached, or refuses to send or take audio.
    pub fn start(
        radio: SocketAddr,
        slice: u8,
        receive: Arc<Buffer>,
        transmit: Option<Arc<Buffer>>,
        on_air: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let mut control = Control::open(radio)?;
        let socket = UdpSocket::bind((IpAddr::from([0, 0, 0, 0]), 0))
            .map_err(|error| format!("can't open a UDP port for the FlexRadio's audio: {error}"))?;
        socket
            .set_read_timeout(Some(Duration::from_millis(100)))
            .map_err(|error| error.to_string())?;
        let port = socket.local_addr().map_err(|e| e.to_string())?.port();
        for command in set_up_commands(slice, port) {
            control.ask(&command)?;
        }
        let rx =
            stream_id(&control.ask(&format!("stream create type=dax_rx dax_channel={CHANNEL}"))?)
                .ok_or("the FlexRadio gave no receive stream number")?;
        let tx = stream_id(&control.ask("stream create type=dax_tx")?)
            .ok_or("the FlexRadio gave no transmit stream number")?;
        let running = Arc::new(AtomicBool::new(true));
        let sender = socket.try_clone().map_err(|error| error.to_string())?;
        let mut threads = Vec::new();
        let flag = Arc::clone(&running);
        threads.push(std::thread::spawn(move || {
            hear(&socket, radio.ip(), rx, &receive, &flag);
        }));
        if let Some(transmit) = transmit {
            let flag = Arc::clone(&running);
            let to = SocketAddr::new(radio.ip(), VITA_PORT);
            threads.push(std::thread::spawn(move || {
                send(&sender, to, tx, transmit, &on_air, &flag);
            }));
        }
        let flag = Arc::clone(&running);
        threads.push(std::thread::spawn(move || keep_alive(control, &flag)));
        Ok(Self { running, threads })
    }
}

/// Takes in the radio's receive audio until `running` turns false.
fn hear(socket: &UdpSocket, radio: IpAddr, stream: u32, into: &Buffer, running: &AtomicBool) {
    let mut packet = vec![0_u8; 2048];
    while running.load(Ordering::SeqCst) {
        let Ok((size, from)) = socket.recv_from(&mut packet) else {
            continue;
        };
        if from.ip() != radio {
            continue;
        }
        if let Some(samples) = samples(&packet[..size], stream) {
            into.push(samples.into_iter());
        }
    }
}

/// Sends a program's transmit audio to the radio, one packet each
/// [`PACKET_SAMPLES`] at [`RATE`], only while `on_air`, until `running`
/// turns false.
fn send(
    socket: &UdpSocket,
    to: SocketAddr,
    stream: u32,
    from: Arc<Buffer>,
    on_air: &AtomicBool,
    running: &AtomicBool,
) {
    let mut resampler = Resampler::new(from, RATE);
    let mut samples = vec![0.0_f32; PACKET_SAMPLES];
    let every = Duration::from_micros(
        u64::try_from(PACKET_SAMPLES).unwrap_or(0) * 1_000_000 / u64::from(RATE),
    );
    let mut count: u8 = 0;
    let mut next = Instant::now();
    while running.load(Ordering::SeqCst) {
        if on_air.load(Ordering::SeqCst) {
            resampler.fill(&mut samples);
            let _ = socket.send_to(&packet(stream, count, &samples), to);
            count = count.wrapping_add(1);
        } else {
            resampler.silence(&mut samples);
        }
        // Kept to the radio's clock on average: each packet is due a
        // packet's time after the last, however late this one ran.
        next += every;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else if now - next > every * 8 {
            next = now;
        }
    }
}

/// Keeps the audio connection open, asking the radio now and then whether
/// it is there, until `running` turns false or the radio goes.
fn keep_alive(mut control: Control, running: &AtomicBool) {
    let mut pinged = Instant::now();
    while running.load(Ordering::SeqCst) {
        if control.line().is_err() {
            crate::print_event("FlexRadio audio: the radio closed the audio connection.");
            return;
        }
        if pinged.elapsed() >= PING_EVERY {
            pinged = Instant::now();
            if control.ask("ping").is_err() {
                crate::print_event("FlexRadio audio: the radio stopped answering.");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn audio_packets_go_out_and_come_back() {
        let tone: Vec<f32> = (0..PACKET_SAMPLES)
            .map(|n| (f32::from(u16::try_from(n).unwrap_or(0)) / 20.0).sin() * 0.5)
            .collect();
        let sent = packet(0x0400_0008, 17, &tone);
        // Seven header words, then two bytes a sample.
        assert_eq!(sent.len(), 28 + PACKET_SAMPLES * 2);
        assert_eq!(&sent[..4], &[0x18, 0xD1, 0x00, 71]);
        let back = samples(&sent, 0x0400_0008).unwrap_or_default();
        assert_eq!(back.len(), PACKET_SAMPLES);
        assert!(back.iter().zip(&tone).all(|(a, b)| (a - b).abs() < 0.001));
        // Another stream's, a meter packet, a short one: not believed.
        assert_eq!(samples(&sent, 0x0400_0009), None);
        let mut meters = sent.clone();
        meters[15] = 0x02;
        meters[14] = 0x80;
        assert_eq!(samples(&meters, 0x0400_0008), None);
        assert_eq!(samples(&sent[..40], 0x0400_0008), None);
        // Loud samples are clipped, not wrapped.
        let loud = samples(&packet(1, 0, &[2.0, -2.0]), 1).unwrap_or_default();
        assert!(loud[0] > 0.99 && loud[1] < -0.99);
    }

    #[test]
    fn reads_the_radios_answers() {
        assert_eq!(stream_id("0x04000008"), Some(0x0400_0008));
        assert_eq!(stream_id("4000009"), Some(0x0400_0009));
        assert_eq!(stream_id(""), None);
        assert_eq!(stream_id("0"), None);
        assert_eq!(
            answer_to("R7|0|0x04000008", 7),
            Some(Ok("0x04000008".to_owned()))
        );
        assert_eq!(answer_to("R7|5000002C|", 7), Some(Err(0x5000_002C)));
        assert_eq!(answer_to("R8|0|", 7), None);
        assert_eq!(answer_to("S1|slice 0 dax=1", 7), None);
        assert_eq!(
            set_up_commands(0, 50_000),
            [
                "client udpport 50000",
                "client set send_reduced_bw_dax=true",
                "slice set 0 dax=1",
                "dax audio set 1 slice=0 tx=1",
            ]
        );
    }

    /// A radio that answers every command with OK, and `stream create`
    /// with a stream number, then sends one audio packet to the client's
    /// UDP port.
    fn pretend_radio() -> Option<(SocketAddr, JoinHandle<Vec<String>>)> {
        let listener = TcpListener::bind("127.0.0.1:0").ok()?;
        let address = listener.local_addr().ok()?;
        let handle = std::thread::spawn(move || {
            let mut heard = Vec::new();
            let Ok((stream, _)) = listener.accept() else {
                return heard;
            };
            let mut writer = stream.try_clone().ok();
            let mut port = 0;
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { break };
                let Some((seq, command)) = line.strip_prefix('C').and_then(|l| l.split_once('|'))
                else {
                    continue;
                };
                if let Some(p) = command.strip_prefix("client udpport ") {
                    port = p.parse().unwrap_or(0);
                }
                let message = match command {
                    "stream create type=dax_rx dax_channel=1" => "0x04000008",
                    "stream create type=dax_tx" => "0x84000000",
                    _ => "",
                };
                heard.push(command.to_owned());
                if let Some(writer) = writer.as_mut() {
                    let _ = writeln!(writer, "R{seq}|0|{message}");
                }
                if command == "stream create type=dax_tx" {
                    if let Ok(udp) = UdpSocket::bind("127.0.0.1:0") {
                        let _ = udp.send_to(
                            &packet(0x0400_0008, 0, &[0.25; PACKET_SAMPLES]),
                            ("127.0.0.1", port),
                        );
                    }
                    break;
                }
            }
            heard
        });
        Some((address, handle))
    }

    #[test]
    fn sends_transmit_audio_only_while_keyed() {
        let (Ok(radio), Ok(agent)) = (
            UdpSocket::bind("127.0.0.1:0"),
            UdpSocket::bind("127.0.0.1:0"),
        ) else {
            return;
        };
        let _ = radio.set_read_timeout(Some(Duration::from_millis(300)));
        let Ok(to) = radio.local_addr() else { return };
        let from = Buffer::new(48_000);
        let on_air = Arc::new(AtomicBool::new(false));
        let running = Arc::new(AtomicBool::new(true));
        let (feed, air, run) = (Arc::clone(&from), Arc::clone(&on_air), Arc::clone(&running));
        let sender = std::thread::spawn(move || send(&agent, to, 0x8400_0000, feed, &air, &run));
        let mut packet = vec![0_u8; 2048];
        // Not keyed: a program's audio is thrown away, and nothing is sent.
        from.push(std::iter::repeat_n(0.5, 48_000));
        assert!(radio.recv(&mut packet).is_err(), "sent while not keyed");
        // Keyed: packets of the transmit stream come.
        on_air.store(true, Ordering::SeqCst);
        let until = Instant::now() + Duration::from_secs(2);
        let mut heard = None;
        while heard.is_none() && Instant::now() < until {
            from.push(std::iter::repeat_n(0.5, 4_800));
            if let Ok(size) = radio.recv(&mut packet) {
                heard = samples(&packet[..size], 0x8400_0000);
            }
        }
        assert_eq!(heard.map(|samples| samples.len()), Some(PACKET_SAMPLES));
        running.store(false, Ordering::SeqCst);
        let _ = sender.join();
    }

    #[test]
    fn sets_the_radio_up_and_hears_its_audio() {
        let Some((address, radio)) = pretend_radio() else {
            return;
        };
        let receive = Buffer::new(RATE);
        let audio = FlexAudio::start(
            address,
            0,
            Arc::clone(&receive),
            None,
            Arc::new(AtomicBool::new(false)),
        );
        assert!(audio.is_ok(), "{:?}", audio.as_ref().err());
        let heard = radio.join().unwrap_or_default();
        assert_eq!(heard.len(), 6, "{heard:?}");
        assert_eq!(heard[5], "stream create type=dax_tx");
        let until = Instant::now() + Duration::from_secs(2);
        while receive.level() == Duration::ZERO && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(receive.level() > Duration::ZERO, "no audio heard");
        drop(audio);
    }
}
