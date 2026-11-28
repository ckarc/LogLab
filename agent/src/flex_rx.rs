// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! The audio of the slice SmartSDR has open on this computer, carried by
//! `FlexTest` itself between the radio and "LabLog Radio": its receive
//! audio into the card's line input (milestone 2 of the sound card plan),
//! and what apps play to the card's speakers to the radio as its DAX
//! transmit audio (milestone 3). There is no DAX program.
//!
//! It never keys the radio (Principle IX): the transmit audio is only
//! what the radio uses while something else keys it with DAX as its
//! transmit source. Our transmit stream is made with `stream create
//! type=dax_tx`; on a bound connection the FLEX-8400 used such a stream's
//! audio on the air at FL-1 on 2026-11-22.
//!
//! A FlexRadio sends a slice's audio over the network to any client that
//! asks for it (`stream create type=dax_rx dax_channel=N`). The slice has
//! to be on that DAX channel (`slice set N dax=1`). The connection binds to
//! SmartSDR on this computer first (`client bind`), because on a FLEX-8400
//! on 4.3.19 an unbound connection sees a default context (found at FL-1,
//! 2026-11-22). It then picks the one slice SmartSDR owns (by
//! `client_handle`, never by letter). If it had to move the slice onto
//! channel 1, it puts the slice back on its own channel when it stops.
//!
//! The audio arrives as VITA-49 packets on our own UDP port
//! (`client udpport`). With `client set send_reduced_bw_dax=true`, each
//! packet holds 128 mono samples at 24 kHz, 16-bit big-endian, with
//! packet class 0x534C0123. Those figures come from nDAX (MIT,
//! <https://github.com/kc2g-flex-tools/nDAX>, `main.go`), an open-source
//! DAX replacement, read for the facts and not copied (Principle XVI).
//! The Agent carried audio this way on FL-1 on 2026-11-22 ("level good").
//! Every packet is checked before it is believed (Principle XII): it must
//! come from the radio, carry our stream's number and the audio class, and
//! be the size it says.
//!
//! The samples go into a [`Buffer`] at 24 kHz. The card takes them out at
//! its own 48 kHz through a [`Resampler`], which also rides out the drift
//! between the radio's clock and this computer's.
//!
//! Windows Firewall must let `FlexTest` take UDP from the LAN, as it does
//! for the Agent on FL-1.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::audio::{Buffer, Resampler};
use crate::flex::{self, Line};
use crate::flex_find::{ClientNews, Discovered, Look, client_news};
use crate::flex_levels::{self, MeterIds, Step};

/// The radio's audio rate, in the reduced form asked for.
pub const RATE: u32 = 24_000;
/// The rate the transmit audio arrives at: the card's.
pub const TRANSMIT_FROM_RATE: u32 = crate::sound_card::RATE;
/// Samples in one audio packet, in the reduced form.
pub const PACKET_SAMPLES: usize = 128;
/// The packet class code of reduced-bandwidth DAX audio.
const AUDIO_CLASS: u16 = 0x0123;
/// The class ID words of an audio packet: FlexRadio's OUI, then its
/// information class and the packet class.
const CLASS_ID: [u32; 2] = [0x0000_1C2D, 0x534C_0123];
/// The radio's UDP port for VITA-49 streams.
const VITA_PORT: u16 = 4991;
/// The DAX channel used.
pub const CHANNEL: u8 = 1;
/// How long connecting, and each answer, may take.
const ANSWER_WAIT: Duration = Duration::from_secs(5);
/// How long one read waits.
const READ_WAIT: Duration = Duration::from_millis(100);
/// How often the radio is asked whether it is still there.
const PING_EVERY: Duration = Duration::from_secs(5);
/// The longest line read from the radio.
const MAX_LINE: usize = 8192;
/// The largest UDP packet taken in: meter packets carry every meter.
const MAX_PACKET: usize = 65_536;

/// The samples of one audio packet for `stream`, from -1.0 to 1.0, or
/// `None` for any other packet.
#[must_use]
pub fn samples(packet: &[u8], stream: u32) -> Option<Vec<f32>> {
    let id = packet.get(4..8)?;
    if u32::from_be_bytes(<[u8; 4]>::try_from(id).ok()?) != stream {
        return None;
    }
    let payload = flex::vita_payload(packet, AUDIO_CLASS)?;
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

/// One transmit audio packet for the radio: `samples` (at most
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
    // Clamped first, so the cast loses nothing; not a number is silence.
    if sample.is_nan() {
        return 0;
    }
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

/// A client handle as status lines give it, such as `0x38F7B742`.
fn handle(text: &str) -> Option<u32> {
    let hex = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))?;
    u32::from_str_radix(hex, 16).ok()
}

/// The slice SmartSDR's client `owner` has open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OurSlice {
    /// The radio's number for it.
    pub number: u8,
    /// Its letter, as SmartSDR shows it.
    pub letter: String,
    /// The DAX channel it was on before (`0` is none).
    pub dax_before: String,
    /// Whether the transmitter took its audio from DAX before (`1`), as
    /// SmartSDR's DAX button in its P/CW panel shows.
    pub tx_dax_before: String,
}

/// The one slice `owner` has open among `slices`.
///
/// # Errors
///
/// It has none, or more than one, so which to hear can't be told.
pub fn our_slice(
    slices: &BTreeMap<u8, BTreeMap<String, String>>,
    owner: u32,
) -> Result<OurSlice, String> {
    let ours: Vec<_> = slices
        .iter()
        .filter(|(_, values)| {
            values.get("client_handle").and_then(|text| handle(text)) == Some(owner)
        })
        .collect();
    match ours.as_slice() {
        [(number, values)] => Ok(OurSlice {
            number: **number,
            letter: values
                .get("index_letter")
                .cloned()
                .unwrap_or_else(|| "?".to_owned()),
            dax_before: values.get("dax").cloned().unwrap_or_else(|| "0".to_owned()),
            tx_dax_before: "1".to_owned(),
        }),
        [] => Err("SmartSDR on this computer has no slice open".to_owned()),
        more => Err(format!(
            "SmartSDR on this computer has {} slices open: close all but one",
            more.len()
        )),
    }
}

/// The commands that start the slice's audio coming, after binding and
/// finding `slice`, with our UDP port `port`.
#[must_use]
pub fn set_up_commands(slice: &OurSlice, port: u16) -> Vec<String> {
    let mut commands = vec![
        format!("client udpport {port}"),
        "sub meter all".to_owned(),
        "client set send_reduced_bw_dax=true".to_owned(),
    ];
    if slice.dax_before != CHANNEL.to_string() {
        commands.push(format!("slice set {} dax={CHANNEL}", slice.number));
    }
    if slice.tx_dax_before != "1" {
        commands.push("transmit set dax=1".to_owned());
    }
    commands.push(format!("stream create type=dax_rx dax_channel={CHANNEL}"));
    commands.push(TX_STREAM.to_owned());
    commands
}

/// The command that makes our transmit stream.
const TX_STREAM: &str = "stream create type=dax_tx";

/// The commands that stop it and put the slice back: the streams go, and
/// the slice returns to the channel it was on, if it was moved.
#[must_use]
pub fn put_back_commands(slice: &OurSlice, streams: Streams) -> Vec<String> {
    let mut commands = vec![
        format!("stream remove 0x{:08X}", streams.tx),
        format!("stream remove 0x{:08X}", streams.rx),
    ];
    if slice.dax_before != CHANNEL.to_string() {
        commands.push(format!(
            "slice set {} dax={}",
            slice.number, slice.dax_before
        ));
    }
    if slice.tx_dax_before != "1" {
        commands.push(format!("transmit set dax={}", slice.tx_dax_before));
    }
    commands
}

/// The answer to command `seq` in `line`: its message, or the code it was
/// refused with. `None` for any other line.
fn answer_to(line: &str, seq: u32) -> Option<Result<String, u32>> {
    match flex::classify(line) {
        Line::Answer { seq: s, code, text } if s == seq => Some(if code == 0 {
            Ok(text.to_owned())
        } else {
            Err(code)
        }),
        _ => None,
    }
}

/// The radio's API connection: numbered commands, their answers, and the
/// status lines in between.
struct Control {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    seq: u32,
    pending: String,
    look: Look,
    /// Which meters are forward power and ALC.
    meters: MeterIds,
}

impl Control {
    fn open(radio: SocketAddr) -> Result<Self, String> {
        let stream = TcpStream::connect_timeout(&radio, ANSWER_WAIT)
            .map_err(|error| format!("can't reach the FlexRadio at {radio}: {error}"))?;
        stream
            .set_read_timeout(Some(READ_WAIT))
            .map_err(|error| error.to_string())?;
        let writer = stream.try_clone().map_err(|error| error.to_string())?;
        let mut control = Self {
            reader: BufReader::new(stream),
            writer,
            seq: 0,
            pending: String::new(),
            look: Look::default(),
            meters: MeterIds::default(),
        };
        let deadline = Instant::now() + ANSWER_WAIT;
        while Instant::now() < deadline {
            if let Some(line) = control.line()?
                && matches!(flex::classify(&line), Line::Version(_))
            {
                return Ok(control);
            }
        }
        Err(format!("{radio} didn't answer like a FlexRadio"))
    }

    /// Sends `command` and waits for its answer, taking in status lines
    /// meanwhile: the answer's message, or why the radio refused it.
    fn ask(&mut self, command: &str) -> Result<String, String> {
        self.seq = self.seq.wrapping_add(1);
        let seq = self.seq;
        writeln!(self.writer, "C{seq}|{command}")
            .and_then(|()| self.writer.flush())
            .map_err(|error| format!("lost touch with the FlexRadio: {error}"))?;
        let deadline = Instant::now() + ANSWER_WAIT;
        while Instant::now() < deadline {
            let Some(line) = self.line()? else { continue };
            if let Some(answer) = answer_to(&line, seq) {
                return answer.map_err(|code| {
                    format!("the FlexRadio refused {command:?}: {}", flex::refusal(code))
                });
            }
            if let Line::Status(status) = flex::classify(&line) {
                if let Some(meters) = status.strip_prefix("meter ") {
                    self.meters.take(meters);
                } else {
                    self.look.take(status);
                }
            }
        }
        Err(format!("the FlexRadio didn't answer {command:?}"))
    }

    /// The next line from the radio, if one came in time. A line cut off
    /// by the wait is kept and finished on the next call.
    fn line(&mut self) -> Result<Option<String>, String> {
        let room = MAX_LINE.saturating_sub(self.pending.len()).max(1);
        let read = (&mut self.reader)
            .take(u64::try_from(room).unwrap_or(u64::MAX))
            .read_line(&mut self.pending);
        match read {
            Ok(0) => Err("the FlexRadio closed the connection".to_owned()),
            Ok(_) if self.pending.ends_with('\n') || self.pending.len() >= MAX_LINE => {
                let line = self.pending.trim_end().to_owned();
                self.pending.clear();
                Ok(Some(line))
            }
            Ok(_) => Ok(None),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(format!("lost touch with the FlexRadio: {error}")),
        }
    }
}

/// The radio's numbers for our two audio streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Streams {
    pub rx: u32,
    pub tx: u32,
}

/// What the audio has done so far.
#[derive(Default)]
pub struct Counts {
    /// Receive audio packets believed.
    pub packets: AtomicU64,
    /// Packets from the radio that weren't ours, or were malformed.
    pub refused: AtomicU64,
    /// Transmit audio packets sent.
    pub sent: AtomicU64,
}

/// The levels `FlexTest` sets itself ([`flex_levels`]), shared by the
/// threads and shown in the log.
pub struct Levels {
    pub tx: Mutex<flex_levels::TxDrive>,
    pub rx: Mutex<flex_levels::RxLevel>,
    /// Forward power last measured, as `f64` bits.
    forward_watts: AtomicU64,
    /// The most forward power since the last log line, as `f64` bits.
    most_watts: AtomicU64,
    /// What the transmit drive did, for the log.
    pub steps: Mutex<Vec<Step>>,
    /// Which meters were found.
    pub meters: MeterIds,
    started: Instant,
}

impl Levels {
    fn new(meters: MeterIds) -> Self {
        Self {
            tx: Mutex::default(),
            rx: Mutex::default(),
            forward_watts: AtomicU64::new(0),
            most_watts: AtomicU64::new(0),
            steps: Mutex::default(),
            meters,
            started: Instant::now(),
        }
    }

    /// Forward power last measured, in watts.
    #[must_use]
    pub fn forward_watts(&self) -> f64 {
        f64::from_bits(self.forward_watts.load(Ordering::Relaxed))
    }

    fn on_air(&self) -> bool {
        self.forward_watts() >= flex_levels::ON_AIR_WATTS
    }

    /// One line for the log: the drive, the receive gain, power, and the
    /// drive's steps since the last line.
    #[must_use]
    pub fn describe(&self) -> String {
        let drive = self.tx.lock().map_or_else(
            |_| "drive unknown".to_owned(),
            |tx| {
                format!(
                    "drive {:.0} dBFS{}",
                    tx.drive_db(),
                    if tx.settled() { " (settled)" } else { "" }
                )
            },
        );
        let rx = self.rx.lock().map_or_else(
            |_| "receive gain unknown".to_owned(),
            |rx| {
                rx.wsjtx_noise_db().map_or_else(
                    || format!("receive gain {:+.1} dB (measuring the noise)", rx.gain_db()),
                    |reads| {
                        format!(
                            "receive gain {:+.1} dB, band noise reads {reads:.0} dB on WSJT-X's scale",
                            rx.gain_db()
                        )
                    },
                )
            },
        );
        let steps: Vec<String> = self
            .steps
            .lock()
            .map(|mut steps| steps.drain(..).map(|step| format!("{step:?}")).collect())
            .unwrap_or_default();
        let meters = match (self.meters.forward, self.meters.alc) {
            (Some(_), Some(_)) => "",
            (Some(_), None) => ", no ALC meter found",
            _ => ", no power meter found",
        };
        format!(
            "{drive}, {rx}, forward {:.1} W (most since last line {:.1} W), steps [{}]{meters}",
            self.forward_watts(),
            f64::from_bits(self.most_watts.swap(0, Ordering::Relaxed)),
            steps.join(", ")
        )
    }

    /// Takes in one meter packet's readings.
    fn meter_readings(&self, readings: &[(u16, i16)]) {
        let find = |id: Option<u16>| {
            id.and_then(|id| readings.iter().find(|(n, _)| *n == id).map(|(_, v)| *v))
        };
        let Some(forward) = find(self.meters.forward).map(flex_levels::watts) else {
            return;
        };
        self.forward_watts
            .store(forward.to_bits(), Ordering::Relaxed);
        if forward > f64::from_bits(self.most_watts.load(Ordering::Relaxed)) {
            self.most_watts.store(forward.to_bits(), Ordering::Relaxed);
        }
        let alc = find(self.meters.alc).map(flex_levels::db);
        let step = self
            .tx
            .lock()
            .ok()
            .and_then(|mut tx| tx.meters(forward, alc, self.started.elapsed()));
        if let Some(step) = step
            && let Ok(mut steps) = self.steps.lock()
            && steps.len() < 1000
        {
            steps.push(step);
        }
    }
}

/// The slice's audio coming in and going out, until [`FlexRx::stop`].
pub struct FlexRx {
    /// The slice heard.
    pub slice: OurSlice,
    /// SmartSDR's client ID, which the audio connection is bound to.
    pub client_id: String,
    /// The radio's numbers for the audio streams.
    pub streams: Streams,
    /// Where the receive audio goes, at [`RATE`].
    pub buffer: Arc<Buffer>,
    /// Where the transmit audio comes from, at [`TRANSMIT_FROM_RATE`].
    pub transmit: Arc<Buffer>,
    /// What has come in and gone out.
    pub counts: Arc<Counts>,
    /// The levels it sets.
    pub levels: Arc<Levels>,
    running: Arc<AtomicBool>,
    hearing: Option<JoinHandle<()>>,
    sending: Option<JoinHandle<()>>,
    /// The connection's thread, which puts the slice back as it ends.
    talking: Option<JoinHandle<Vec<String>>>,
}

impl FlexRx {
    /// Connects to `radio`, binds to SmartSDR's client `owner`, and starts
    /// the audio of the one slice it owns.
    ///
    /// # Errors
    ///
    /// The radio can't be reached, SmartSDR isn't among its clients, it has
    /// no slice or several, or the radio refuses a command.
    pub fn start(radio: SocketAddr, owner: u32) -> Result<Self, String> {
        let mut control = Control::open(radio)?;
        control.ask("sub client all")?;
        let client_id = control
            .look
            .clients
            .iter()
            .find(|client| client.handle == owner)
            .map(|client| client.client_id.clone())
            .ok_or("SmartSDR on this computer isn't among the radio's clients")?;
        control.ask(&format!("client bind client_id={client_id}"))?;
        control.look.bound = Some(owner);
        control.ask("sub slice all")?;
        control.ask("sub tx all")?;
        let mut slice = our_slice(&control.look.slices, owner)?;
        "0".clone_into(&mut slice.tx_dax_before);
        if control
            .look
            .transmit
            .get("dax")
            .is_some_and(|dax| dax == "1")
        {
            "1".clone_into(&mut slice.tx_dax_before);
        }

        let socket = UdpSocket::bind((IpAddr::from([0, 0, 0, 0]), 0))
            .map_err(|error| format!("can't open a UDP port for the audio: {error}"))?;
        socket
            .set_read_timeout(Some(READ_WAIT))
            .map_err(|error| error.to_string())?;
        let port = socket.local_addr().map_err(|e| e.to_string())?.port();
        let (mut rx, mut tx) = (None, None);
        for command in set_up_commands(&slice, port) {
            let answer = control.ask(&command)?;
            if command == TX_STREAM {
                tx = stream_id(&answer);
            } else if command.starts_with("stream create") {
                rx = stream_id(&answer);
            }
        }
        let streams = Streams {
            rx: rx.ok_or("the FlexRadio gave no receive stream number")?,
            tx: tx.ok_or("the FlexRadio gave no transmit stream number")?,
        };
        let sender = socket.try_clone().map_err(|error| error.to_string())?;

        let buffer = Buffer::new(RATE);
        let transmit = Buffer::new(TRANSMIT_FROM_RATE);
        let levels = Arc::new(Levels::new(control.meters));
        let counts = Arc::new(Counts::default());
        let running = Arc::new(AtomicBool::new(true));
        let hearing = {
            let (buffer, counts, levels, running) = (
                Arc::clone(&buffer),
                Arc::clone(&counts),
                Arc::clone(&levels),
                Arc::clone(&running),
            );
            std::thread::spawn(move || {
                let into = Into {
                    buffer: &buffer,
                    counts: &counts,
                    levels: &levels,
                };
                hear(&socket, radio.ip(), streams.rx, &into, &running);
            })
        };
        let sending = {
            let (transmit, counts, levels, running) = (
                Arc::clone(&transmit),
                Arc::clone(&counts),
                Arc::clone(&levels),
                Arc::clone(&running),
            );
            let to = SocketAddr::new(radio.ip(), VITA_PORT);
            std::thread::spawn(move || {
                send(
                    &sender, to, streams.tx, transmit, &counts, &levels, &running,
                );
            })
        };
        let talking = {
            let (slice, running) = (slice.clone(), Arc::clone(&running));
            std::thread::spawn(move || keep_alive(control, &slice, streams, &running))
        };
        Ok(Self {
            slice,
            client_id,
            streams,
            buffer,
            transmit,
            counts,
            levels,
            running,
            hearing: Some(hearing),
            sending: Some(sending),
            talking: Some(talking),
        })
    }

    /// Stops the audio and puts the slice back: what was done, line by
    /// line, for the log.
    pub fn stop(mut self) -> Vec<String> {
        self.halt()
    }

    fn halt(&mut self) -> Vec<String> {
        self.running.store(false, Ordering::SeqCst);
        for thread in [self.hearing.take(), self.sending.take()]
            .into_iter()
            .flatten()
        {
            let _ = thread.join();
        }
        self.talking
            .take()
            .map(|talking| talking.join().unwrap_or_default())
            .unwrap_or_default()
    }

    /// Whether the connection to the radio is still up.
    #[must_use]
    pub fn connected(&self) -> bool {
        self.talking
            .as_ref()
            .is_some_and(|talking| !talking.is_finished())
    }
}

impl Drop for FlexRx {
    fn drop(&mut self) {
        let _ = self.halt();
    }
}

/// Where the radio's audio and meters go.
struct Into<'a> {
    buffer: &'a Buffer,
    counts: &'a Counts,
    levels: &'a Levels,
}

/// One packet's length in seconds.
#[allow(clippy::cast_precision_loss)]
const PACKET_SECONDS: f64 = PACKET_SAMPLES as f64 / RATE as f64;

/// Takes in the radio's audio and meters until `running` turns false.
fn hear(socket: &UdpSocket, radio: IpAddr, stream: u32, into: &Into<'_>, running: &AtomicBool) {
    let mut packet = vec![0_u8; MAX_PACKET];
    while running.load(Ordering::SeqCst) {
        let Ok((size, from)) = socket.recv_from(&mut packet) else {
            continue;
        };
        if from.ip() != radio {
            continue;
        }
        let packet = &packet[..size];
        if let Some(mut samples) = samples(packet, stream) {
            into.counts.packets.fetch_add(1, Ordering::Relaxed);
            let on_air = into.levels.on_air();
            if let Ok(mut rx) = into.levels.rx.lock() {
                rx.apply(&mut samples, PACKET_SECONDS, on_air);
            }
            into.buffer.push(samples.into_iter());
        } else {
            let readings = flex::meter_readings(packet);
            if readings.is_empty() {
                into.counts.refused.fetch_add(1, Ordering::Relaxed);
            } else {
                into.levels.meter_readings(&readings);
            }
        }
    }
}

/// Sends what apps play to the card to the radio, one packet each
/// [`PACKET_SAMPLES`] at [`RATE`], until `running` turns false. When apps
/// play nothing, the packets are silence.
fn send(
    socket: &UdpSocket,
    to: SocketAddr,
    stream: u32,
    from: Arc<Buffer>,
    counts: &Counts,
    levels: &Levels,
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
        resampler.fill(&mut samples);
        // Driven to the level the radio wants, whatever the app's own.
        let peak = samples.iter().fold(0.0_f32, |most, s| most.max(s.abs()));
        let gain = levels.tx.lock().map_or(1.0, |mut tx| {
            tx.app_peak(peak, levels.started.elapsed());
            tx.gain()
        });
        for sample in &mut samples {
            *sample *= gain;
        }
        if socket.send_to(&packet(stream, count, &samples), to).is_ok() {
            counts.sent.fetch_add(1, Ordering::Relaxed);
        }
        count = count.wrapping_add(1);
        // Kept to the radio's rate on average: each packet is due a
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

/// Keeps the connection open, asking the radio now and then whether it is
/// there, until `running` turns false or the radio goes; then puts the
/// slice back. What it did, for the log.
fn keep_alive(
    mut control: Control,
    slice: &OurSlice,
    streams: Streams,
    running: &AtomicBool,
) -> Vec<String> {
    let mut pinged = Instant::now();
    while running.load(Ordering::SeqCst) {
        match control.line() {
            Err(why) => return vec![format!("Flex audio stopped: {why}")],
            Ok(Some(line)) => {
                if let Line::Status(status) = flex::classify(&line)
                    && let Some(ClientNews::Gone(gone)) = client_news(status)
                    && control.look.bound == Some(gone)
                {
                    return vec!["Flex audio stopped: SmartSDR disconnected".to_owned()];
                }
            }
            Ok(None) => {}
        }
        if pinged.elapsed() >= PING_EVERY {
            pinged = Instant::now();
            if let Err(why) = control.ask("ping") {
                return vec![format!("Flex audio stopped: {why}")];
            }
        }
    }
    put_back_commands(slice, streams)
        .into_iter()
        .map(|command| match control.ask(&command) {
            Ok(_) => format!("Flex: {command}"),
            Err(why) => format!("Flex: {command} failed: {why}"),
        })
        .collect()
}

/// The radio to hear, from what discovery heard: the one SmartSDR on this
/// computer uses, and its client handle there. `here` finds this
/// computer's address toward each radio.
///
/// # Errors
///
/// No radio was heard, or none lists SmartSDR on this computer.
pub fn pick(
    heard: &[Discovered],
    here: impl Fn(IpAddr) -> Option<IpAddr>,
) -> Result<(SocketAddr, u32, String), String> {
    if heard.is_empty() {
        return Err(
            "no FlexRadio heard. Is it on, and does Windows Firewall let FlexTest take UDP from the LAN?"
                .to_owned(),
        );
    }
    heard
        .iter()
        .find_map(|radio| {
            radio
                .client_here(here(radio.address.ip()))
                .map(|owner| (radio.address, owner, radio.model.clone()))
        })
        .ok_or_else(|| {
            "no FlexRadio heard lists SmartSDR on this computer: is SmartSDR connected?".to_owned()
        })
}

#[cfg(test)]
mod tests {
    use super::{
        FlexRx, OurSlice, PACKET_SAMPLES, Streams, answer_to, our_slice, pick, put_back_commands,
        samples, set_up_commands, stream_id,
    };
    use crate::flex_find::{Discovered, Seen};
    use std::collections::BTreeMap;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{IpAddr, SocketAddr, TcpListener, UdpSocket};
    use std::time::{Duration, Instant};

    /// A radio on 127.0.0.1 that lists FL-1's SmartSDR and its slice,
    /// answers every command, sends one audio packet to the UDP port it is
    /// given once the stream is made, and hands back every command heard.
    fn pretend_radio() -> Option<(SocketAddr, std::thread::JoinHandle<Vec<String>>)> {
        let listener = TcpListener::bind("127.0.0.1:0").ok()?;
        let address = listener.local_addr().ok()?;
        let radio = std::thread::spawn(move || {
            let mut heard = Vec::new();
            let Ok((stream, _)) = listener.accept() else {
                return heard;
            };
            let Ok(mut writer) = stream.try_clone() else {
                return heard;
            };
            let _ = writeln!(writer, "V1.4.0.0");
            let _ = writeln!(writer, "H12345678");
            let mut port = 0;
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { break };
                let Some((seq, command)) = line.strip_prefix('C').and_then(|l| l.split_once('|'))
                else {
                    continue;
                };
                heard.push(command.to_owned());
                let mut answer = String::new();
                match command {
                    "sub client all" => {
                        let _ = writeln!(
                            writer,
                            "S0|client 0x38F7B742 connected local_ptt=1 client_id=114C1CFC-0000 program=SmartSDR-Win station=FL-1"
                        );
                    }
                    "sub slice all" => {
                        let _ = writeln!(
                            writer,
                            "S0|slice 0 in_use=1 client_handle=0x38F7B742 index_letter=A dax=0"
                        );
                    }
                    "sub tx all" => {
                        let _ = writeln!(writer, "S0|transmit rfpower=10 dax=0");
                    }
                    "stream create type=dax_rx dax_channel=1" => answer = "0x04000008".to_owned(),
                    "stream create type=dax_tx" => answer = "0x84000000".to_owned(),
                    _ => {}
                }
                if let Some(udp) = command.strip_prefix("client udpport ") {
                    port = udp.parse().unwrap_or(0);
                }
                let _ = writeln!(writer, "R{seq}|0|{answer}");
                if command.starts_with("stream create type=dax_rx") {
                    let tone: Vec<i16> = (0..128)
                        .map(|n| if n % 2 == 0 { 8000 } else { -8000 })
                        .collect();
                    if let Ok(udp) = UdpSocket::bind("127.0.0.1:0") {
                        let _ = udp.send_to(&packet(0x0400_0008, &tone), ("127.0.0.1", port));
                        let _ = udp.send_to(&packet(0x0400_0009, &tone), ("127.0.0.1", port));
                    }
                }
                if command.starts_with("transmit set dax=0") {
                    break;
                }
            }
            heard
        });
        Some((address, radio))
    }

    #[test]
    fn hears_the_slice_then_puts_it_back() -> Result<(), String> {
        let (address, radio) = pretend_radio().ok_or("no pretend radio")?;
        let rx = FlexRx::start(address, 0x38F7_B742)?;
        assert_eq!(rx.slice.letter, "A");
        assert_eq!(rx.client_id, "114C1CFC-0000");
        assert_eq!(rx.slice.tx_dax_before, "0");
        assert_eq!(
            rx.streams,
            Streams {
                rx: 0x0400_0008,
                tx: 0x8400_0000
            }
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while rx.counts.packets.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            assert!(Instant::now() < deadline, "no audio came in");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(rx.connected());
        // One packet's 128 samples at 24 kHz, 5.3 ms; the other stream's
        // packet was refused.
        let level = rx.buffer.level();
        assert!(level > Duration::from_millis(5) && level < Duration::from_millis(6));
        // Transmit packets go out all along, silence when nothing plays;
        // what apps play is taken from `transmit`.
        rx.transmit.push(std::iter::repeat_n(0.25, 48_000 / 5));
        while rx.counts.sent.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            assert!(Instant::now() < deadline, "no transmit audio went out");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(rx.levels.forward_watts() < crate::flex_levels::ON_AIR_WATTS);
        assert!(
            rx.levels
                .describe()
                .starts_with("drive -10 dBFS, receive gain")
        );
        let done = rx.stop();
        assert_eq!(
            done,
            [
                "Flex: stream remove 0x84000000",
                "Flex: stream remove 0x04000008",
                "Flex: slice set 0 dax=0",
                "Flex: transmit set dax=0"
            ]
        );
        let heard = radio.join().map_err(|_| "the pretend radio failed")?;
        assert_eq!(
            heard,
            [
                "sub client all",
                "client bind client_id=114C1CFC-0000",
                "sub slice all",
                "sub tx all",
                &format!(
                    "client udpport {}",
                    heard[4].trim_start_matches("client udpport ")
                ),
                "sub meter all",
                "client set send_reduced_bw_dax=true",
                "slice set 0 dax=1",
                "transmit set dax=1",
                "stream create type=dax_rx dax_channel=1",
                "stream create type=dax_tx",
                "stream remove 0x84000000",
                "stream remove 0x04000008",
                "slice set 0 dax=0",
                "transmit set dax=0",
            ]
        );
        Ok(())
    }

    #[test]
    fn logs_the_most_forward_power_since_the_last_line() {
        let levels = super::Levels::new(super::MeterIds {
            forward: Some(8),
            alc: Some(30),
        });
        // 40 dBm (10 W), then 0 dBm (1 mW): raw values are 1/128 dB.
        levels.meter_readings(&[(8, 40 * 128), (30, -6 * 128)]);
        levels.meter_readings(&[(8, 0), (30, -60 * 128)]);
        let line = levels.describe();
        assert!(
            line.contains("forward 0.0 W (most since last line 10.0 W)"),
            "{line}"
        );
        let next = levels.describe();
        assert!(next.contains("(most since last line 0.0 W)"), "{next}");
    }

    #[test]
    fn picks_the_radio_smartsdr_here_uses() {
        let here: IpAddr = [10, 73, 10, 56].into();
        let radio = |ip: [u8; 4], client: Option<[u8; 4]>| Discovered {
            model: "FLEX-8400".to_owned(),
            serial: "1".to_owned(),
            version: None,
            name: None,
            address: SocketAddr::new(ip.into(), 4992),
            clients: Some(
                client
                    .map(|c| Seen {
                        ip: c.into(),
                        host: None,
                        program: None,
                        station: None,
                        handle: Some(0x38F7_B742),
                    })
                    .into_iter()
                    .collect(),
            ),
        };
        let heard = [
            radio([10, 73, 10, 200], Some([10, 73, 10, 57])),
            radio([10, 73, 10, 233], Some([10, 73, 10, 56])),
        ];
        let picked = pick(&heard, |_| Some(here));
        assert_eq!(
            picked,
            Ok((
                "10.73.10.233:4992".parse().unwrap_or(heard[0].address),
                0x38F7_B742,
                "FLEX-8400".to_owned()
            ))
        );
        assert!(pick(&heard[..1], |_| Some(here)).is_err());
        assert!(pick(&[], |_| Some(here)).is_err());
    }

    /// One audio packet as the radio sends it: IF data with a stream ID,
    /// class ID, timestamps, then 16-bit samples.
    fn packet(stream: u32, samples: &[i16]) -> Vec<u8> {
        let words = 7 + samples.len() / 2;
        let header = 0x18D0_0000 | u32::try_from(words).unwrap_or(0);
        let mut packet: Vec<u8> = [header, stream, 0x0000_1C2D, 0x534C_0123, 0, 0, 0]
            .iter()
            .flat_map(|word| word.to_be_bytes())
            .collect();
        for sample in samples {
            packet.extend_from_slice(&sample.to_be_bytes());
        }
        packet
    }

    #[test]
    fn only_our_streams_audio_is_believed() {
        let sent: Vec<i16> = (0..128).map(|n| n * 100 - 6400).collect();
        let got = samples(&packet(0x0400_0008, &sent), 0x0400_0008).unwrap_or_default();
        assert_eq!(got.len(), PACKET_SAMPLES);
        assert!((got[0] - (-6400.0 / 32768.0)).abs() < 1e-6);
        // Another stream's, a meter packet, a cut-off one: not believed.
        assert_eq!(samples(&packet(0x0400_0009, &sent), 0x0400_0008), None);
        let mut meter = packet(0x0400_0008, &sent);
        meter[14] = 0x80;
        meter[15] = 0x02;
        assert_eq!(samples(&meter, 0x0400_0008), None);
        assert_eq!(
            samples(&packet(0x0400_0008, &sent)[..40], 0x0400_0008),
            None
        );
    }

    #[test]
    fn transmit_packets_carry_the_samples_in_the_radios_layout() {
        let tone: Vec<f32> = (0..PACKET_SAMPLES)
            .map(|n| (f32::from(u16::try_from(n).unwrap_or(0)) / 20.0).sin() * 0.5)
            .collect();
        let sent = super::packet(0x8400_0000, 17, &tone);
        // Seven header words, then two bytes a sample; count 17 sends 1.
        assert_eq!(sent.len(), 28 + PACKET_SAMPLES * 2);
        assert_eq!(&sent[..4], &[0x18, 0xD1, 0x00, 71]);
        let back = samples(&sent, 0x8400_0000).unwrap_or_default();
        assert_eq!(back.len(), PACKET_SAMPLES);
        assert!(back.iter().zip(&tone).all(|(a, b)| (a - b).abs() < 0.001));
        // Loud samples are clipped, not wrapped; not a number is silence.
        let loud =
            samples(&super::packet(1, 0, &[2.0, -2.0, f32::NAN, 0.0]), 1).unwrap_or_default();
        assert!(loud[0] > 0.99 && loud[1] < -0.99);
        assert!(loud[2].abs() < f32::EPSILON);
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
    }

    fn slices(list: &[(u8, &str)]) -> BTreeMap<u8, BTreeMap<String, String>> {
        list.iter()
            .map(|(number, text)| {
                let values = text
                    .split(' ')
                    .filter_map(|word| word.split_once('='))
                    .map(|(k, v)| (k.to_owned(), v.to_owned()))
                    .collect();
                (*number, values)
            })
            .collect()
    }

    #[test]
    fn hears_only_the_slice_smartsdr_here_owns() {
        // As at FL-1: slice 0 is FL-1's SmartSDR's; slice 1 another PC's.
        let all = slices(&[
            (0, "client_handle=0x38F7B742 index_letter=A dax=0 tx=1"),
            (1, "client_handle=0x11111111 index_letter=A dax=1"),
        ]);
        assert_eq!(
            our_slice(&all, 0x38F7_B742),
            Ok(OurSlice {
                number: 0,
                letter: "A".to_owned(),
                dax_before: "0".to_owned(),
                tx_dax_before: "1".to_owned(),
            })
        );
        assert!(our_slice(&all, 0x2222_2222).is_err());
        let two = slices(&[
            (0, "client_handle=0x38F7B742 index_letter=A"),
            (1, "client_handle=0x38F7B742 index_letter=B"),
        ]);
        assert!(our_slice(&two, 0x38F7_B742).is_err_and(|why| why.contains("2 slices")));
    }

    #[test]
    fn moves_the_slice_to_channel_1_only_if_needed_and_puts_it_back() {
        let moved = OurSlice {
            number: 0,
            letter: "A".to_owned(),
            dax_before: "0".to_owned(),
            tx_dax_before: "0".to_owned(),
        };
        assert_eq!(
            set_up_commands(&moved, 50_000),
            [
                "client udpport 50000",
                "sub meter all",
                "client set send_reduced_bw_dax=true",
                "slice set 0 dax=1",
                "transmit set dax=1",
                "stream create type=dax_rx dax_channel=1",
                "stream create type=dax_tx",
            ]
        );
        let streams = Streams {
            rx: 0x0400_0008,
            tx: 0x8400_0000,
        };
        assert_eq!(
            put_back_commands(&moved, streams),
            [
                "stream remove 0x84000000",
                "stream remove 0x04000008",
                "slice set 0 dax=0",
                "transmit set dax=0"
            ]
        );
        let already = OurSlice {
            dax_before: "1".to_owned(),
            tx_dax_before: "1".to_owned(),
            ..moved
        };
        assert_eq!(
            set_up_commands(&already, 50_000),
            [
                "client udpport 50000",
                "sub meter all",
                "client set send_reduced_bw_dax=true",
                "stream create type=dax_rx dax_channel=1",
                "stream create type=dax_tx",
            ]
        );
        assert_eq!(
            put_back_commands(&already, streams),
            ["stream remove 0x84000000", "stream remove 0x04000008"]
        );
    }
}
