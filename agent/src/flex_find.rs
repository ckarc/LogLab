// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Finding FlexRadio radios on the network, and seeing the slice SmartSDR
//! has open on this computer. Read-only: nothing here changes the radio.
//!
//! Each FlexRadio broadcasts a discovery packet on UDP 4992 every second
//! (the API wiki's Discovery Protocol page). SmartSDR holds that port while
//! it runs, so the Agent shares it (`SO_REUSEADDR`, and `SO_REUSEPORT` on
//! Unix). Windows Firewall must also let the Agent take inbound UDP from
//! the LAN; FL-1 has that rule.
//!
//! Whether a radio is in use is judged from the GUI clients the broadcast
//! lists (`gui_client_ips`, `gui_client_programs`, `gui_client_stations`,
//! `gui_client_handles`), never from its `status`: a FLEX-8400 on 4.3.19
//! said `status=Available` while SmartSDR on FL-1 used it (seen 2026-11-22).
//!
//! To see SmartSDR's slice and transmit settings, the Agent connects to the
//! radio's API, lists the GUI clients (`sub client all`), and binds to the
//! one on this computer (`client bind client_id=...`). FlexRadio's wiki says
//! binding "performs no function in the radio" since v3.0, but on a
//! FLEX-8400 on 4.3.19 an unbound connection sees a default transmit
//! context (dax=0, rfpower=100) while a bound one sees SmartSDR's own
//! (seen at FL-1, 2026-11-22). Binding changes nothing in the radio; it
//! ends when the connection closes.

use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

use crate::flex::{self, Line};

/// The UDP port FlexRadio radios broadcast their discovery packets to.
pub const DISCOVERY_PORT: u16 = 4992;
/// The VITA-49 packet class code of a discovery broadcast.
const DISCOVERY_CLASS: u16 = 0xFFFF;
/// How long the `flex` command listens for broadcasts (each radio sends one
/// every second).
pub const LISTEN: Duration = Duration::from_secs(3);
/// The most radios kept from one listen.
const MOST_RADIOS: usize = 16;
/// The most GUI clients kept for one radio.
const MOST_CLIENTS: usize = 8;
/// The most slices or transmit band lines kept.
const MOST_SLICES: usize = 16;
const MOST_BANDS: usize = 32;
/// The most `key=value`s kept from one status line.
const MOST_VALUES: usize = 256;
/// The longest key or value kept.
const LONGEST_VALUE: usize = 64;
/// The longest line read from the radio.
const LONGEST_LINE: usize = 8192;
/// How long connecting and each answer may take.
const ANSWER_WAIT: Duration = Duration::from_secs(5);
/// How long one read waits.
const READ_WAIT: Duration = Duration::from_millis(100);

/// One GUI client (SmartSDR, Maestro...) a discovery broadcast lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    /// The computer it runs on.
    pub ip: IpAddr,
    /// That computer's host name, if given.
    pub host: Option<String>,
    /// The program, such as "SmartSDR-Win".
    pub program: Option<String>,
    /// Its station name, such as "FL-1".
    pub station: Option<String>,
    /// Its client handle.
    pub handle: Option<u32>,
}

/// A FlexRadio that said it is on the network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Discovered {
    /// Its model, such as "FLEX-8400".
    pub model: String,
    /// Its serial number.
    pub serial: String,
    /// Its software version.
    pub version: Option<String>,
    /// Its nickname, if it has one.
    pub name: Option<String>,
    /// Where its API listens.
    pub address: SocketAddr,
    /// The GUI clients using it; `None` when the broadcast says nothing
    /// about them, so whether it is in use can't be told.
    pub clients: Option<Vec<Seen>>,
}

impl Discovered {
    /// Whether a computer other than `here` uses it: `None` when the radio
    /// doesn't say.
    #[must_use]
    pub fn used_elsewhere(&self, here: Option<IpAddr>) -> Option<bool> {
        let clients = self.clients.as_ref()?;
        Some(clients.iter().any(|client| Some(client.ip) != here))
    }

    /// The handle of the GUI client running on `here`, if the broadcast
    /// names one.
    #[must_use]
    pub fn client_here(&self, here: Option<IpAddr>) -> Option<u32> {
        let here = here?;
        self.clients
            .as_ref()?
            .iter()
            .find(|client| client.ip == here)
            .and_then(|client| client.handle)
    }
}

/// Whether a word from the radio is plain enough to keep and show.
fn plain(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= LONGEST_VALUE
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}

/// A client handle as the radio writes it, `0x38F7B742`.
fn handle(text: &str) -> Option<u32> {
    u32::from_str_radix(text.strip_prefix("0x")?, 16).ok()
}

/// The GUI clients a broadcast lists. `gui_client_ips` and its fellows are
/// comma lists in the same order (seen from a FLEX-8400 on 4.3.19:
/// `gui_client_ips=10.73.10.56 gui_client_hosts=fl1.fieldlab.lan
/// gui_client_programs=SmartSDR-Win gui_client_stations=FL-1
/// gui_client_handles=0x38F7B742`). Without them, `inuse_ip` names one
/// computer. Neither: `None`.
fn clients_from(fields: &BTreeMap<&str, &str>) -> Option<Vec<Seen>> {
    let list = |key: &str| -> Vec<&str> {
        fields
            .get(key)
            .map(|value| value.split(',').collect())
            .unwrap_or_default()
    };
    let word = |words: &[&str], at: usize| {
        words
            .get(at)
            .copied()
            .filter(|w| plain(w))
            .map(str::to_owned)
    };
    if let Some(ips) = fields.get("gui_client_ips") {
        let (hosts, programs, stations, handles) = (
            list("gui_client_hosts"),
            list("gui_client_programs"),
            list("gui_client_stations"),
            list("gui_client_handles"),
        );
        return Some(
            ips.split(',')
                .enumerate()
                .filter_map(|(at, ip)| {
                    Some(Seen {
                        ip: ip.trim().parse().ok()?,
                        host: word(&hosts, at),
                        program: word(&programs, at),
                        station: word(&stations, at),
                        handle: handles.get(at).and_then(|h| handle(h)),
                    })
                })
                .take(MOST_CLIENTS)
                .collect(),
        );
    }
    let ip = fields.get("inuse_ip")?;
    Some(
        ip.parse()
            .ok()
            .map(|ip| Seen {
                ip,
                host: fields
                    .get("inuse_host")
                    .copied()
                    .filter(|h| plain(h))
                    .map(str::to_owned),
                program: None,
                station: None,
                handle: None,
            })
            .into_iter()
            .collect(),
    )
}

/// What a discovery broadcast says: `key=value` words in a VITA-49 packet
/// with class code 0xFFFF (the API wiki's Discovery Protocol page; a
/// name's spaces are sent as underscores). `None` unless it names a plain
/// model and serial and a usable address.
#[must_use]
pub fn discovery(packet: &[u8]) -> Option<Discovered> {
    let payload = flex::vita_payload(packet, DISCOVERY_CLASS)?;
    let text = std::str::from_utf8(payload).ok()?.trim_end_matches('\0');
    let mut fields: BTreeMap<&str, &str> = BTreeMap::new();
    for (key, value) in text.split(' ').filter_map(|word| word.split_once('=')) {
        fields.entry(key).or_insert(value);
    }
    let get = |key: &str| fields.get(key).copied().filter(|t| plain(t));
    let model = get("model")?;
    let serial = get("serial")?;
    let ip: IpAddr = fields.get("ip")?.parse().ok()?;
    let port = fields
        .get("port")
        .map_or(Some(flex::PORT), |port| port.parse().ok())?;
    Some(Discovered {
        model: model.to_owned(),
        serial: serial.to_owned(),
        version: get("version").map(str::to_owned),
        name: get("nickname")
            .or_else(|| get("name"))
            .map(|name| name.replace('_', " ")),
        address: SocketAddr::new(ip, port),
        clients: clients_from(&fields),
    })
}

/// This computer's address on the way to `radio`: what the radio sees its
/// clients here come from. Nothing is sent.
#[must_use]
pub fn here_toward(radio: IpAddr) -> Option<IpAddr> {
    let socket = UdpSocket::bind((IpAddr::from([0, 0, 0, 0]), 0)).ok()?;
    socket.connect((radio, DISCOVERY_PORT)).ok()?;
    socket.local_addr().ok().map(|address| address.ip())
}

/// Listens for `wait` and says which radios were heard, once each.
///
/// # Errors
///
/// The discovery port can't be opened even shared.
pub fn discover(wait: Duration) -> Result<Vec<Discovered>, String> {
    let socket = shared_socket().map_err(|error| {
        format!(
            "can't listen for FlexRadio radios on UDP port {DISCOVERY_PORT} ({error}); \
             another program may hold it alone"
        )
    })?;
    heard_on(&socket, wait)
}

/// A socket on the discovery port that shares it with SmartSDR. On
/// Windows, `SO_REUSEADDR` set before the bind lets it bind beside
/// SmartSDR; tried on FL-1 (SmartSDR 4.3.19, 2026-11-22): without it the
/// bind fails (error 10048), with it every broadcast is heard. On macOS and
/// the BSDs sharing a port also takes `SO_REUSEPORT`.
fn shared_socket() -> io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, SockAddr, Socket, Type};
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    let here = SocketAddr::new(IpAddr::from([0, 0, 0, 0]), DISCOVERY_PORT);
    socket.bind(&SockAddr::from(here))?;
    Ok(socket.into())
}

/// The radios heard on `socket` within `wait`, the newest broadcast of each.
fn heard_on(socket: &UdpSocket, wait: Duration) -> Result<Vec<Discovered>, String> {
    socket
        .set_read_timeout(Some(READ_WAIT))
        .map_err(|error| error.to_string())?;
    let deadline = Instant::now() + wait;
    let mut heard: Vec<Discovered> = Vec::new();
    let mut packet = vec![0_u8; 2048];
    while Instant::now() < deadline {
        let Ok(size) = socket.recv(&mut packet) else {
            continue;
        };
        let Some(radio) = discovery(&packet[..size]) else {
            continue;
        };
        if let Some(known) = heard.iter_mut().find(|h| h.serial == radio.serial) {
            *known = radio;
        } else if heard.len() < MOST_RADIOS {
            heard.push(radio);
        }
    }
    Ok(heard)
}

/// A GUI client from the radio's own list (`sub client all`), such as
/// `client 0x38F7B742 connected local_ptt=1 client_id=114C1CFC-...
/// program=SmartSDR-Win station=FL-1` (FLEX-8400, 4.3.19). Non-GUI
/// connections, like the Agent's, are not listed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuiClient {
    pub handle: u32,
    pub client_id: String,
    pub program: Option<String>,
    pub station: Option<String>,
}

/// What one `client` status line says.
#[derive(Debug, PartialEq, Eq)]
pub enum ClientNews {
    Connected(GuiClient),
    Gone(u32),
}

/// Reads one `client ...` status line.
#[must_use]
pub fn client_news(status: &str) -> Option<ClientNews> {
    let mut words = status.strip_prefix("client ")?.split(' ');
    let id = handle(words.next()?)?;
    match words.next()? {
        "disconnected" => Some(ClientNews::Gone(id)),
        "connected" => {
            let mut fields: BTreeMap<&str, &str> = BTreeMap::new();
            for (key, value) in words.filter_map(|word| word.split_once('=')) {
                fields.entry(key).or_insert(value);
            }
            let get = |key: &str| fields.get(key).copied().filter(|t| plain(t));
            Some(ClientNews::Connected(GuiClient {
                handle: id,
                client_id: get("client_id")?.to_owned(),
                program: get("program").map(str::to_owned),
                station: get("station").map(str::to_owned),
            }))
        }
        _ => None,
    }
}

/// A status line's `key=value`s, kept only if plain enough.
fn values(text: &str) -> BTreeMap<String, String> {
    let keepable = |t: &str| {
        !t.is_empty() && t.len() <= LONGEST_VALUE && t.bytes().all(|b| b.is_ascii_graphic())
    };
    text.split(' ')
        .filter_map(|word| word.split_once('='))
        .filter(|(k, v)| keepable(k) && (v.is_empty() || keepable(v)))
        .take(MOST_VALUES)
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

/// What the Agent saw through its connection to the radio.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Look {
    /// The GUI clients the radio listed.
    pub clients: Vec<GuiClient>,
    /// The GUI client the Agent bound to.
    pub bound: Option<u32>,
    /// Each slice's latest values, by slice number.
    pub slices: BTreeMap<u8, BTreeMap<String, String>>,
    /// The transmitter's values (`transmit freq=...` and its fellows).
    pub transmit: BTreeMap<String, String>,
    /// Each band's own powers (`transmit band N ...`), by band number.
    pub bands: BTreeMap<u16, BTreeMap<String, String>>,
}

impl Look {
    /// Takes in one status line.
    pub fn take(&mut self, status: &str) {
        if let Some(news) = client_news(status) {
            match news {
                ClientNews::Connected(client) => {
                    self.clients.retain(|c| c.handle != client.handle);
                    if self.clients.len() < MOST_CLIENTS {
                        self.clients.push(client);
                    }
                }
                ClientNews::Gone(gone) => self.clients.retain(|c| c.handle != gone),
            }
        } else if let Some(rest) = status.strip_prefix("slice ") {
            let (number, rest) = rest.split_once(' ').unwrap_or((rest, ""));
            let Ok(number) = number.parse::<u8>() else {
                return;
            };
            if rest.split(' ').any(|w| w == "in_use=0") {
                self.slices.remove(&number);
                return;
            }
            if !self.slices.contains_key(&number) && self.slices.len() >= MOST_SLICES {
                return;
            }
            self.slices.entry(number).or_default().extend(values(rest));
        } else if let Some(rest) = status.strip_prefix("transmit band ") {
            let (number, rest) = rest.split_once(' ').unwrap_or((rest, ""));
            let Ok(number) = number.parse::<u16>() else {
                return;
            };
            if !self.bands.contains_key(&number) && self.bands.len() >= MOST_BANDS {
                return;
            }
            self.bands.entry(number).or_default().extend(values(rest));
        } else if let Some(rest) = status.strip_prefix("transmit ") {
            self.transmit.extend(values(rest));
            if self.transmit.len() > MOST_VALUES {
                self.transmit.clear();
            }
        }
    }

    /// The slices SmartSDR's bound client owns, or every slice when none
    /// is bound.
    fn shown_slices(&self) -> impl Iterator<Item = (&u8, &BTreeMap<String, String>)> {
        self.slices.iter().filter(|(_, slice)| match self.bound {
            Some(bound) => slice.get("client_handle").and_then(|h| handle(h)) == Some(bound),
            None => true,
        })
    }
}

/// The commands the `flex` command may send. None changes the radio.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Question {
    Clients,
    Bind(String),
    Slices,
    Transmitter,
}

impl Question {
    /// The command as the API takes it.
    #[must_use]
    pub fn command(&self) -> String {
        match self {
            Self::Clients => "sub client all".to_owned(),
            Self::Bind(id) => format!("client bind client_id={id}"),
            Self::Slices => "sub slice all".to_owned(),
            Self::Transmitter => "sub tx all".to_owned(),
        }
    }
}

/// A short connection to the radio's API, for questions only.
struct Session {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    pending: Vec<u8>,
    seq: u32,
    look: Look,
}

impl Session {
    fn open(address: SocketAddr) -> Result<Self, String> {
        let stream = TcpStream::connect_timeout(&address, ANSWER_WAIT)
            .map_err(|error| format!("cannot reach the FlexRadio at {address}: {error}"))?;
        stream
            .set_read_timeout(Some(READ_WAIT))
            .map_err(|error| error.to_string())?;
        let writer = stream.try_clone().map_err(|error| error.to_string())?;
        let mut session = Self {
            reader: BufReader::new(stream),
            writer,
            pending: Vec::new(),
            seq: 0,
            look: Look::default(),
        };
        let deadline = Instant::now() + ANSWER_WAIT;
        loop {
            if Instant::now() > deadline {
                return Err(format!("{address} did not answer like a FlexRadio"));
            }
            if let Some(line) = session.next_line()?
                && matches!(flex::classify(&line), Line::Version(_))
            {
                return Ok(session);
            }
        }
    }

    /// Asks one question and waits for its answer, taking in status lines
    /// meanwhile. The answer's code and text.
    fn ask(&mut self, question: &Question) -> Result<(u32, String), String> {
        self.seq = self.seq.wrapping_add(1);
        let command = question.command();
        self.writer
            .write_all(format!("C{}|{command}\n", self.seq).as_bytes())
            .map_err(|error| format!("lost touch with the FlexRadio: {error}"))?;
        let deadline = Instant::now() + ANSWER_WAIT;
        loop {
            if Instant::now() > deadline {
                return Err(format!("the FlexRadio did not answer {command:?}"));
            }
            let Some(line) = self.next_line()? else {
                continue;
            };
            match flex::classify(&line) {
                Line::Answer { seq, code, text } if seq == self.seq => {
                    return Ok((code, text.to_owned()));
                }
                Line::Status(status) => self.look.take(status),
                _ => {}
            }
        }
    }

    /// The next whole line, or `None` when nothing more has arrived yet.
    fn next_line(&mut self) -> Result<Option<String>, String> {
        let room = u64::try_from(LONGEST_LINE + 1 - self.pending.len()).unwrap_or(0);
        match (&mut self.reader)
            .take(room)
            .read_until(b'\n', &mut self.pending)
        {
            Ok(0) => Err("the FlexRadio closed the connection".to_owned()),
            Ok(_) if self.pending.last() == Some(&b'\n') => {
                let mut bytes = std::mem::take(&mut self.pending);
                bytes.pop();
                let line = String::from_utf8(bytes)
                    .map_err(|_| "the FlexRadio sent something that is not text".to_owned())?;
                Ok(Some(line.trim_end_matches('\r').to_owned()))
            }
            Ok(_) if self.pending.len() > LONGEST_LINE => {
                Err("the FlexRadio sent a line that is too long".to_owned())
            }
            Ok(_) => Ok(None),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(format!("lost touch with the FlexRadio: {error}")),
        }
    }
}

/// Connects to the radio at `address`, lists its GUI clients, and when
/// `here` (the handle of the GUI client on this computer) is among them,
/// binds to it and reads its slices and transmit settings.
///
/// # Errors
///
/// The radio can't be reached, doesn't answer, or refuses a question.
pub fn look(address: SocketAddr, here: Option<u32>) -> Result<Look, String> {
    let mut session = Session::open(address)?;
    let refused = |question: &Question, code: u32| {
        format!(
            "the FlexRadio refused {:?}: {}",
            question.command(),
            flex::refusal(code)
        )
    };
    let (code, _) = session.ask(&Question::Clients)?;
    if code != 0 {
        return Err(refused(&Question::Clients, code));
    }
    let Some(client) = here.and_then(|handle| {
        session
            .look
            .clients
            .iter()
            .find(|client| client.handle == handle)
            .cloned()
    }) else {
        return Ok(session.look);
    };
    let bind = Question::Bind(client.client_id.clone());
    let (code, _) = session.ask(&bind)?;
    if code != 0 {
        return Err(refused(&bind, code));
    }
    session.look.bound = Some(client.handle);
    for question in [Question::Slices, Question::Transmitter] {
        let (code, _) = session.ask(&question)?;
        if code != 0 {
            return Err(refused(&question, code));
        }
    }
    Ok(session.look)
}

/// One radio, as the `flex` command shows it.
#[must_use]
pub fn describe(radio: &Discovered, here: Option<IpAddr>) -> Vec<String> {
    let mut lines = vec![format!(
        "{}{} serial {} at {}{}",
        radio.model,
        radio
            .name
            .as_ref()
            .map(|n| format!(" \"{n}\""))
            .unwrap_or_default(),
        radio.serial,
        radio.address.ip(),
        radio
            .version
            .as_ref()
            .map(|v| format!(", software {v}"))
            .unwrap_or_default(),
    )];
    match &radio.clients {
        None => lines.push("  in use: the radio doesn't say".to_owned()),
        Some(clients) if clients.is_empty() => {
            lines.push("  in use: no, free for any computer".to_owned());
        }
        Some(clients) => {
            for client in clients {
                let what = client.program.as_deref().unwrap_or("a program");
                let station = client
                    .station
                    .as_ref()
                    .map(|s| format!(" (station {s})"))
                    .unwrap_or_default();
                let place = if Some(client.ip) == here {
                    "this computer".to_owned()
                } else {
                    format!(
                        "ANOTHER computer, {}{}",
                        client.ip,
                        client
                            .host
                            .as_ref()
                            .map(|h| format!(" {h}"))
                            .unwrap_or_default()
                    )
                };
                lines.push(format!("  in use: {what}{station} on {place}"));
            }
        }
    }
    lines.push(
        match radio.used_elsewhere(here) {
            Some(false) => "  free for this computer: yes",
            Some(true) => "  free for this computer: NO, another computer uses it",
            None => "  free for this computer: can't tell",
        }
        .to_owned(),
    );
    lines
}

/// What the Agent saw through SmartSDR's binding, as the `flex` command
/// shows it.
#[must_use]
pub fn describe_look(look: &Look, heard: bool) -> Vec<String> {
    let Some(bound) = look.bound else {
        let mut lines: Vec<String> = look
            .clients
            .iter()
            .map(|client| {
                format!(
                    "  the radio lists {}{}",
                    client.program.as_deref().unwrap_or("a GUI client"),
                    client
                        .station
                        .as_ref()
                        .map(|s| format!(" (station {s})"))
                        .unwrap_or_default()
                )
            })
            .collect();
        lines.push(if !heard {
            "  its broadcast wasn't heard, so which SmartSDR runs on this computer can't be \
             told; Windows Firewall must let this program take UDP 4992 from the LAN"
                .to_owned()
        } else if look.clients.is_empty() {
            "  SmartSDR is not open on any computer, so there is no slice to see".to_owned()
        } else {
            "  SmartSDR is not open on this computer, so its slice can't be seen from here"
                .to_owned()
        });
        return lines;
    };
    let mut lines = vec![format!(
        "  following SmartSDR on this computer (client 0x{bound:08X})"
    )];
    let value = |map: &BTreeMap<String, String>, key: &str| {
        map.get(key).cloned().unwrap_or_else(|| "?".to_owned())
    };
    let mut any = false;
    for (number, slice) in look.shown_slices() {
        any = true;
        let flag = |key: &str, word: &str| {
            if slice.get(key).map(String::as_str) == Some("1") {
                format!(" {word}")
            } else {
                String::new()
            }
        };
        lines.push(format!(
            "  slice {} ({number}): {} MHz {} filter {}-{} Hz, DAX channel {}{}{}",
            value(slice, "index_letter"),
            value(slice, "RF_frequency"),
            value(slice, "mode"),
            value(slice, "filter_lo"),
            value(slice, "filter_hi"),
            value(slice, "dax"),
            flag("tx", "TRANSMIT"),
            flag("active", "active"),
        ));
    }
    if !any {
        lines.push("  SmartSDR has no slice open".to_owned());
    }
    if look.transmit.contains_key("rfpower") {
        let t = &look.transmit;
        let dax = match t.get("dax").map(String::as_str) {
            Some("1") => "on",
            Some("0") => "off (transmit audio from the mic)",
            _ => "?",
        };
        lines.push(format!(
            "  transmit: {} MHz {}, power {} W, tune power {} W, DAX {dax}",
            value(t, "freq"),
            value(t, "tx_slice_mode"),
            value(t, "rfpower"),
            value(t, "tunepower"),
        ));
    }
    let bands: Vec<String> = look
        .bands
        .values()
        .filter_map(|band| {
            Some(format!(
                "{} m {} W",
                band.get("band_name")?,
                band.get("rfpower")?
            ))
        })
        .collect();
    if !bands.is_empty() {
        lines.push(format!("  power by band: {}", bands.join(", ")));
    }
    lines
}

/// `flex [ADDRESS]`: lists the FlexRadio radios heard on the network and
/// who uses each, then shows SmartSDR's slice on this computer for the one
/// radio (or the one at `address`). Changes nothing.
///
/// # Errors
///
/// Discovery can't listen, or the radio can't be reached or refuses.
pub fn run(address: Option<&str>) -> Result<(), String> {
    println!("Listening {} s for FlexRadio radios...", LISTEN.as_secs());
    let heard = discover(LISTEN)?;
    for radio in &heard {
        for line in describe(radio, here_toward(radio.address.ip())) {
            println!("{line}");
        }
    }
    let chosen: Option<SocketAddr> = match address {
        Some(text) => Some(parse_address(text)?),
        None => match heard.as_slice() {
            [] => {
                return Err(
                    "no FlexRadio heard. Is it on, and does Windows Firewall let the Agent \
                     take UDP 4992 from the LAN? Or give its address: flex 192.168.1.20"
                        .to_owned(),
                );
            }
            [one] => Some(one.address),
            _ => {
                println!("More than one radio: say which, as flex ADDRESS.");
                None
            }
        },
    };
    let Some(chosen) = chosen else {
        return Ok(());
    };
    let radio = heard.iter().find(|r| r.address.ip() == chosen.ip());
    if radio.is_none() {
        println!("{} was not heard; asking its API.", chosen.ip());
    }
    let here = radio.and_then(|r| r.client_here(here_toward(chosen.ip())));
    let look = look(chosen, here)?;
    for line in describe_look(&look, radio.is_some()) {
        println!("{line}");
    }
    Ok(())
}

/// An address given as `IP` or `IP:PORT`.
fn parse_address(text: &str) -> Result<SocketAddr, String> {
    text.parse::<SocketAddr>()
        .or_else(|_| {
            text.parse::<IpAddr>()
                .map(|ip| SocketAddr::new(ip, flex::PORT))
        })
        .map_err(|_| format!("{text:?} is not an IP address, such as 192.168.1.20"))
}

#[cfg(test)]
mod tests {
    use super::{
        ClientNews, Discovered, GuiClient, Look, Question, Seen, client_news, describe,
        describe_look, discovery, heard_on, look, shared_socket,
    };
    use std::io::{BufRead, BufReader, Write};
    use std::net::{IpAddr, SocketAddr, TcpListener};
    use std::thread;
    use std::time::Duration;

    /// The FLEX-8400's broadcast at FL-1 (2026-11-22), as heard.
    const FL1_BROADCAST: &str = "discovery_protocol_version=3.1.0.4 model=FLEX-8400 \
        serial=1726-1213-8402-6545 version=4.3.19.42208 nickname= callsign= ip=10.73.10.233 \
        port=4992 status=Available inuse_ip=10.73.10.56 inuse_host=fl1.fieldlab.lan \
        max_licensed_version=v4 radio_license_id=00-1C-2D-05-3E-C4 fpc_mac= wan_connected=1 \
        licensed_clients=2 available_clients=1 max_panadapters=2 available_panadapters=1 \
        max_slices=2 available_slices=1 gui_client_ips=10.73.10.56 \
        gui_client_hosts=fl1.fieldlab.lan gui_client_programs=SmartSDR-Win \
        gui_client_stations=FL-1 gui_client_handles=0x38F7B742 min_software_version=2.13.0.0 \
        external_port_link=1 license_is_unknown=0 is_system_model=0 turf_region=USA";

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap_or_else(|_| unreachable!())
    }

    /// A broadcast as the FLEX-8400 sent it: header 0x385500AD (extension
    /// data with stream ID, class ID, both timestamps), stream 0x800, class
    /// 0x00001C2D 0x534CFFFF, three timestamp words, then the text padded
    /// with zeroes to a whole word.
    fn packet(text: &str) -> Vec<u8> {
        let mut payload = text.as_bytes().to_vec();
        payload.extend_from_slice(&[0; 4]);
        payload.truncate(payload.len() / 4 * 4);
        let words = 7 + payload.len() / 4;
        let header = 0x3850_0000 | u32::try_from(words).unwrap_or(0);
        let mut packet: Vec<u8> = [header, 0x800, 0x0000_1C2D, 0x534C_FFFF, 0, 0, 0]
            .iter()
            .flat_map(|w| w.to_be_bytes())
            .collect();
        packet.extend_from_slice(&payload);
        packet
    }

    #[test]
    fn reads_the_fl1_broadcast() {
        let heard = discovery(&packet(FL1_BROADCAST));
        assert_eq!(
            heard,
            Some(Discovered {
                model: "FLEX-8400".to_owned(),
                serial: "1726-1213-8402-6545".to_owned(),
                version: Some("4.3.19.42208".to_owned()),
                name: None,
                address: SocketAddr::new(ip("10.73.10.233"), 4992),
                clients: Some(vec![Seen {
                    ip: ip("10.73.10.56"),
                    host: Some("fl1.fieldlab.lan".to_owned()),
                    program: Some("SmartSDR-Win".to_owned()),
                    station: Some("FL-1".to_owned()),
                    handle: Some(0x38F7_B742),
                }]),
            })
        );
    }

    #[test]
    fn judges_in_use_from_the_clients_not_the_status() {
        let Some(radio) = discovery(&packet(FL1_BROADCAST)) else {
            unreachable!()
        };
        // status=Available, yet SmartSDR on FL-1 uses it.
        assert_eq!(radio.used_elsewhere(Some(ip("10.73.10.56"))), Some(false));
        assert_eq!(radio.used_elsewhere(Some(ip("10.73.10.57"))), Some(true));
        assert_eq!(
            radio.client_here(Some(ip("10.73.10.56"))),
            Some(0x38F7_B742)
        );
        assert_eq!(radio.client_here(Some(ip("10.73.10.57"))), None);
        let shown = describe(&radio, Some(ip("10.73.10.57"))).join("\n");
        assert!(
            shown.contains("ANOTHER computer, 10.73.10.56 fl1.fieldlab.lan"),
            "{shown}"
        );
        assert!(shown.contains("free for this computer: NO"), "{shown}");
        let shown = describe(&radio, Some(ip("10.73.10.56"))).join("\n");
        assert!(
            shown.contains("SmartSDR-Win (station FL-1) on this computer"),
            "{shown}"
        );
        assert!(shown.contains("free for this computer: yes"), "{shown}");

        let free = discovery(&packet(
            "model=FLEX-8400 serial=1 ip=10.0.0.2 status=Available gui_client_ips=",
        ));
        assert_eq!(
            free.as_ref().and_then(|r| r.used_elsewhere(None)),
            Some(false)
        );
        let shown = free
            .map(|r| describe(&r, None).join("\n"))
            .unwrap_or_default();
        assert!(shown.contains("free for any computer"), "{shown}");
        let older = discovery(&packet(
            "model=FLEX-6400 serial=1 ip=10.0.0.2 inuse_ip=10.0.0.9",
        ));
        assert_eq!(older.and_then(|r| r.used_elsewhere(None)), Some(true));
        let silent = discovery(&packet("model=FLEX-6400 serial=1 ip=10.0.0.2"));
        assert_eq!(silent.and_then(|r| r.used_elsewhere(None)), None);
    }

    #[test]
    fn refuses_odd_broadcasts() {
        for bad in [
            "model=FLEX-6600 ip=192.168.1.50",
            "model=FLEX-6600 serial=1 ip=nowhere",
            "model=FLEX\u{1b}[2J serial=1 ip=192.168.1.50",
            "serial=1 ip=192.168.1.50",
        ] {
            assert_eq!(discovery(&packet(bad)), None, "{bad}");
        }
        // A meter packet (class 0x8002) is not a broadcast.
        let mut meters = packet("model=FLEX-6600 serial=1 ip=10.0.0.2");
        meters[14] = 0x80;
        meters[15] = 0x02;
        assert_eq!(discovery(&meters), None);
        // A program name with escapes is dropped, the client kept.
        let odd = discovery(&packet(
            "model=FLEX-8400 serial=1 ip=10.0.0.2 gui_client_ips=10.0.0.9 \
             gui_client_programs=\u{1b}[31m",
        ));
        let clients = odd.and_then(|r| r.clients).unwrap_or_default();
        assert_eq!(clients.len(), 1);
        assert_eq!(clients.first().and_then(|c| c.program.clone()), None);
    }

    #[test]
    fn shares_the_discovery_port_and_hears_each_radio_once() {
        // As SmartSDR holds it: one socket on the port, then the Agent's.
        let first = shared_socket();
        let second = shared_socket();
        assert!(first.is_ok(), "{first:?}");
        assert!(second.is_ok(), "{second:?}");
        let (Ok(listener), Ok(sender)) = (
            std::net::UdpSocket::bind("127.0.0.1:0"),
            std::net::UdpSocket::bind("127.0.0.1:0"),
        ) else {
            return;
        };
        let Ok(to) = listener.local_addr() else {
            return;
        };
        for text in [
            "model=FLEX-6600 serial=A1 ip=192.168.1.50 gui_client_ips=",
            "model=FLEX-6600 serial=A1 ip=192.168.1.50 gui_client_ips=192.168.1.9",
            "model=FLEX-8400 serial=B2 ip=192.168.1.51",
        ] {
            let _ = sender.send_to(&packet(text), to);
        }
        let heard = heard_on(&listener, Duration::from_millis(400)).unwrap_or_default();
        let serials: Vec<&str> = heard.iter().map(|r| r.serial.as_str()).collect();
        assert_eq!(serials, ["A1", "B2"]);
        // The newest broadcast wins.
        assert_eq!(
            heard.first().and_then(|r| r.used_elsewhere(None)),
            Some(true)
        );
    }

    #[test]
    fn reads_the_radios_client_list() {
        assert_eq!(
            client_news(
                "client 0x38F7B742 connected local_ptt=1 \
                 client_id=114C1CFC-2615-497A-BAB8-9315F62B8FAB program=SmartSDR-Win station=FL-1"
            ),
            Some(ClientNews::Connected(GuiClient {
                handle: 0x38F7_B742,
                client_id: "114C1CFC-2615-497A-BAB8-9315F62B8FAB".to_owned(),
                program: Some("SmartSDR-Win".to_owned()),
                station: Some("FL-1".to_owned()),
            }))
        );
        assert_eq!(
            client_news("client 0x38F7B742 disconnected forced=0 wan_ban=0 duplicate_client_id=0"),
            Some(ClientNews::Gone(0x38F7_B742))
        );
        assert_eq!(client_news("client 0x38F7B742 connected program=x"), None);
        assert_eq!(client_news("client nonsense connected client_id=1"), None);
        assert_eq!(client_news("slice 0 tx=1"), None);
    }

    #[test]
    fn asks_only_questions() {
        for question in [
            Question::Clients,
            Question::Bind("1".to_owned()),
            Question::Slices,
            Question::Transmitter,
        ] {
            let command = question.command();
            assert!(
                command.starts_with("sub ") || command.starts_with("client bind "),
                "{command} is not a question"
            );
        }
    }

    /// The lines FL-1's radio sent (2026-11-22), shortened.
    const FL1_SLICE: &str = "slice 0 in_use=1 sample_rate=24000 RF_frequency=29.250000 \
        client_handle=0x38F7B742 index_letter=A rxant=ANT1 mode=DIGU filter_lo=0 filter_hi=3000 \
        dax=1 lock=0 tx=1 active=1";
    const FL1_TRANSMIT: &str = "transmit freq=29.250000 rfpower=100 tunepower=10 \
        tx_slice_mode=DIGU hwalc_enabled=0 inhibit=0 dax=1 mic_selection=MIC compander=1";

    /// A stand-in radio answering as FL-1's did. Returns every command it got.
    fn fake_radio(with_smartsdr: bool) -> Option<(SocketAddr, thread::JoinHandle<Vec<String>>)> {
        let listener = TcpListener::bind("127.0.0.1:0").ok()?;
        let address = listener.local_addr().ok()?;
        let handle = thread::spawn(move || {
            let mut seen = Vec::new();
            let Ok((stream, _)) = listener.accept() else {
                return seen;
            };
            let Ok(mut out) = stream.try_clone() else {
                return seen;
            };
            let _ = out.write_all(
                b"V1.4.0.0\nH6292CF1B\nM10000001|Client connected from IP 10.73.10.10\n\
                  S0|interlock tx_client_handle=0x00000000 state=READY\n",
            );
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { break };
                let (seq, command) = line
                    .trim_start_matches('C')
                    .split_once('|')
                    .unwrap_or_default();
                let reply = match command {
                    "sub client all" if with_smartsdr => format!(
                        "S6292CF1B|client 0x38F7B742 connected local_ptt=1 \
                         client_id=114C1CFC-2615-497A-BAB8-9315F62B8FAB program=SmartSDR-Win \
                         station=FL-1\nR{seq}|0|\n"
                    ),
                    "client bind client_id=114C1CFC-2615-497A-BAB8-9315F62B8FAB" => {
                        format!("S6292CF1B|profile tx current=Default\nR{seq}|0|0x38F7B742\n")
                    }
                    "sub slice all" => format!(
                        "S6292CF1B|{FL1_SLICE}\nS6292CF1B|slice 1 in_use=1 RF_frequency=7.074000 \
                         client_handle=0x11111111 index_letter=B mode=DIGU tx=0\nR{seq}|0|\n"
                    ),
                    "sub tx all" => format!(
                        "S6292CF1B|transmit tx_rf_power_changes_allowed=1 tune=0 \
                         max_power_level=100\nS6292CF1B|{FL1_TRANSMIT}\n\
                         S6292CF1B|transmit band 5 band_name=40 rfpower=10 tunepower=25\n\
                         S6292CF1B|transmit band 16 band_name=10 rfpower=100 tunepower=10\n\
                         R{seq}|0|\n"
                    ),
                    _ => format!("R{seq}|0|\n"),
                };
                seen.push(command.to_owned());
                let _ = out.write_all(reply.as_bytes());
            }
            seen
        });
        Some((address, handle))
    }

    #[test]
    fn binds_to_smartsdr_here_and_shows_its_slice() {
        let Some((address, radio)) = fake_radio(true) else {
            return;
        };
        let seen = look(address, Some(0x38F7_B742));
        let commands = radio.join().unwrap_or_default();
        let Ok(seen) = seen else {
            unreachable!("{seen:?}")
        };
        assert_eq!(
            commands,
            [
                "sub client all",
                "client bind client_id=114C1CFC-2615-497A-BAB8-9315F62B8FAB",
                "sub slice all",
                "sub tx all"
            ]
        );
        assert_eq!(seen.bound, Some(0x38F7_B742));
        let shown = describe_look(&seen, true).join("\n");
        assert!(
            shown.contains(
                "slice A (0): 29.250000 MHz DIGU filter 0-3000 Hz, DAX channel 1 TRANSMIT active"
            ),
            "{shown}"
        );
        // Another client's slice is not SmartSDR's.
        assert!(!shown.contains("7.074000"), "{shown}");
        assert!(
            shown.contains("transmit: 29.250000 MHz DIGU, power 100 W, tune power 10 W, DAX on"),
            "{shown}"
        );
        assert!(
            shown.contains("power by band: 40 m 10 W, 10 m 100 W"),
            "{shown}"
        );
    }

    #[test]
    fn binds_to_nothing_when_smartsdr_is_not_here() {
        // SmartSDR is on the radio, but on another computer.
        let Some((address, radio)) = fake_radio(true) else {
            return;
        };
        let seen = look(address, None);
        let commands = radio.join().unwrap_or_default();
        assert_eq!(commands, ["sub client all"]);
        let Ok(seen) = seen else {
            unreachable!("{seen:?}")
        };
        assert_eq!(seen.clients.len(), 1);
        assert_eq!(seen.bound, None);
        let shown = describe_look(&seen, true).join("\n");
        assert!(
            shown.contains("the radio lists SmartSDR-Win (station FL-1)"),
            "{shown}"
        );
        assert!(shown.contains("not open on this computer"), "{shown}");
        // Its broadcast not heard (Windows Firewall, as at FL-1 with a
        // renamed exe): it can't tell, and says why.
        let shown = describe_look(&seen, false).join("\n");
        assert!(shown.contains("can't be told; Windows Firewall"), "{shown}");
        assert!(
            describe_look(&Look::default(), true)
                .join("\n")
                .contains("not open on any computer")
        );
        // No SmartSDR at all, though discovery said one was here.
        let Some((address, radio)) = fake_radio(false) else {
            return;
        };
        let seen = look(address, Some(0x38F7_B742));
        assert_eq!(radio.join().unwrap_or_default(), ["sub client all"]);
        assert_eq!(seen.map(|s| s.bound), Ok(None));
    }

    #[test]
    fn keeps_slices_and_bands_bounded() {
        let mut seen = Look::default();
        for number in 0..40 {
            seen.take(&format!("slice {number} in_use=1 mode=USB"));
            seen.take(&format!("transmit band {number} band_name=x rfpower=1"));
        }
        assert_eq!(seen.slices.len(), 16);
        assert_eq!(seen.bands.len(), 32);
        seen.take("slice 3 in_use=0");
        assert_eq!(seen.slices.len(), 15);
        seen.take("slice 0 mode=\u{1b}[2J");
        assert_eq!(
            seen.slices
                .get(&0)
                .and_then(|s| s.get("mode"))
                .map(String::as_str),
            Some("USB")
        );
    }
}
