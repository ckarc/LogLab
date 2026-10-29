// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Keying the radio for VARA's client, by standing between them.
//!
//! VARA doesn't key the radio for Winlink Express: when it wants to
//! transmit it sends `PTT ON` (and `PTT OFF` when done) on its TCP command
//! port, each command ending in a carriage return, and its client keys the
//! radio (Pat's VARA modem, <https://github.com/n8jja/Pat-Vara>, `vara.go`:
//! `handleCmd` and `writeCmd`; VARA's author says HF PTT is set in the
//! Winlink program, not in VARA, in the comments on
//! <https://rosmodem.wordpress.com/2017/09/03/vara-hf-modem/>). Winlink
//! Express keys only through its own radio drivers on a COM port, which the
//! Agent holds.
//!
//! So the Agent moves VARA to other ports (see `vara`) and listens on the
//! ones VARA's clients already use: the command port and, one above it, the
//! data port (VARA's data port is always one more than its command port:
//! varanny's README, <https://github.com/islandmagic/varanny>). Every byte
//! is passed through unchanged, both ways. When VARA says `PTT ON`, the
//! Agent keys the radio through its own `rigctld` server, as one more
//! program, so every rule there holds: one program at a time, the time
//! limit, the radio's own PTT wins, and the radio is unkeyed when the
//! client goes (Principle IX).
//!
//! A client that already keys the radio through the Agent's `rigctld`
//! server itself, such as `VarAC`, is left to do so: the Agent keying too
//! would make one of the two refused. Which program a client is comes from
//! `program_id` on Windows.
//!
//! Like the `rigctld` server, the proxy listens on this computer only and
//! refuses connections from anywhere else (Principle XXI), and a command
//! line it reads is capped in length (Principle XII).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// How often waiting threads look at whether to stop.
const TICK: Duration = Duration::from_millis(100);
/// The longest VARA command line kept while looking for `PTT ON`; VARA's
/// commands are a few words.
const MAX_LINE: usize = 1024;
/// How long the Agent waits for its own `rigctld` server to answer.
const ANSWER_WITHIN: Duration = Duration::from_secs(5);

/// Whether the program connected from `client` to the proxy at `proxy`
/// already keys the radio itself, through the Agent's `rigctld` server
/// listening on `rigctld_port`.
pub type KeysItself = Arc<dyn Fn(SocketAddr, SocketAddr, u16) -> bool + Send + Sync>;

/// What VARA asked for on its command port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ptt {
    /// `PTT ON`: VARA is about to transmit.
    On,
    /// `PTT OFF`: VARA is done transmitting.
    Off,
}

/// VARA's command lines, read from the bytes as they arrive.
#[derive(Default)]
pub struct Commands {
    partial: Vec<u8>,
}

impl Commands {
    /// The PTT requests completed by `bytes`, in order.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Ptt> {
        let mut found = Vec::new();
        for &byte in bytes {
            if byte == b'\r' || byte == b'\n' {
                match self.partial.as_slice() {
                    b"PTT ON" => found.push(Ptt::On),
                    b"PTT OFF" => found.push(Ptt::Off),
                    _ => {}
                }
                self.partial.clear();
            } else if self.partial.len() < MAX_LINE {
                self.partial.push(byte);
            } else {
                // Too long to be a command the Agent acts on: ignored to the
                // end of the line.
                self.partial.clear();
                self.partial.extend_from_slice(b"\0");
            }
        }
        found
    }
}

/// The proxy, listening until dropped.
pub struct Proxy {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

/// Where the proxy listens and where it reaches VARA and the radio.
#[derive(Clone)]
pub struct Ports {
    /// The command port VARA's clients connect to; the data port is one more.
    pub clients: u16,
    /// VARA's own command port; its data port is one more.
    pub vara: u16,
    /// The Agent's `rigctld` server.
    pub rigctld: SocketAddr,
}

impl Proxy {
    /// Listens on `ports.clients` and one above it, on this computer only.
    ///
    /// # Errors
    ///
    /// Either port can't be listened on, usually because VARA itself is
    /// still on it.
    pub fn start(
        ports: &Ports,
        keys_itself: KeysItself,
        notes: Sender<String>,
    ) -> Result<Self, String> {
        let listen = |port: u16| {
            let listener = TcpListener::bind(("127.0.0.1", port))
                .map_err(|error| format!("can't listen on 127.0.0.1:{port}: {error}"))?;
            listener
                .set_nonblocking(true)
                .map_err(|error| format!("can't listen on 127.0.0.1:{port}: {error}"))?;
            Ok::<_, String>(listener)
        };
        let data_port = |port: u16| {
            port.checked_add(1)
                .ok_or_else(|| format!("port {port} has no data port above it"))
        };
        let command = listen(ports.clients)?;
        let data = listen(data_port(ports.clients)?)?;
        let vara_data = data_port(ports.vara)?;
        let stop = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();
        {
            let (stop, ports) = (Arc::clone(&stop), ports.clone());
            threads.push(thread::spawn(move || {
                accept(&command, &stop, |client, peer| {
                    let session = Session {
                        ports: ports.clone(),
                        peer,
                        keys_itself: Arc::clone(&keys_itself),
                        notes: notes.clone(),
                        stop: Arc::clone(&stop),
                    };
                    thread::spawn(move || session.run(&client));
                });
            }));
        }
        {
            let stop = Arc::clone(&stop);
            threads.push(thread::spawn(move || {
                accept(&data, &stop, |client, _| {
                    let stop = Arc::clone(&stop);
                    thread::spawn(move || {
                        if let Ok(vara) = connect_vara(vara_data, &stop) {
                            pipe_both_ways(&client, &vara, &stop);
                        }
                    });
                });
            }));
        }
        Ok(Self { stop, threads })
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// Hands each connection from this computer to `take`, until `stop`.
fn accept(listener: &TcpListener, stop: &AtomicBool, mut take: impl FnMut(TcpStream, SocketAddr)) {
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, peer)) if peer.ip().is_loopback() => {
                if stream.set_nonblocking(false).is_ok() {
                    take(stream, peer);
                }
            }
            Ok(_) | Err(_) => thread::sleep(TICK),
        }
    }
}

/// How long the proxy waits for VARA to listen. A program such as `VarAC`
/// starts VARA itself and connects at once; reaching VARA directly, it is
/// refused until VARA listens and tries again, so the proxy, which takes the
/// connection at once, waits for VARA in its place rather than dropping it.
const VARA_STARTS_WITHIN: Duration = Duration::from_secs(30);
/// How often the proxy tries VARA while it starts.
const TRY_VARA_EVERY: Duration = Duration::from_millis(250);

/// Connects to VARA's `port` on this computer, trying again until VARA
/// listens, for at most [`VARA_STARTS_WITHIN`] or until `stop`.
fn connect_vara(port: u16, stop: &AtomicBool) -> std::io::Result<TcpStream> {
    let since = std::time::Instant::now();
    loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(stream) => return Ok(stream),
            Err(error) if since.elapsed() >= VARA_STARTS_WITHIN || stop.load(Ordering::SeqCst) => {
                return Err(error);
            }
            Err(_) => thread::sleep(TRY_VARA_EVERY),
        }
    }
}

/// Copies `from` into `to` until either ends or `stop`, then closes both.
/// `seen` is shown each chunk before it is passed on.
fn copy(from: &TcpStream, to: &TcpStream, stop: &AtomicBool, mut seen: impl FnMut(&[u8])) {
    let (mut from_r, mut to_w) = (from, to);
    let _ = from.set_read_timeout(Some(TICK));
    let mut buffer = [0u8; 4096];
    while !stop.load(Ordering::SeqCst) {
        match from_r.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                let chunk = &buffer[..read];
                seen(chunk);
                if to_w.write_all(chunk).and_then(|()| to_w.flush()).is_err() {
                    break;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
    }
    let _ = from.shutdown(Shutdown::Both);
    let _ = to.shutdown(Shutdown::Both);
}

/// Passes bytes between `a` and `b`, both ways, until either ends.
fn pipe_both_ways(a: &TcpStream, b: &TcpStream, stop: &Arc<AtomicBool>) {
    let (Ok(a2), Ok(b2)) = (a.try_clone(), b.try_clone()) else {
        return;
    };
    let other_stop = Arc::clone(stop);
    let other = thread::spawn(move || copy(&b2, &a2, &other_stop, |_| {}));
    copy(a, b, stop, |_| {});
    let _ = other.join();
}

/// One client on VARA's command port.
struct Session {
    ports: Ports,
    peer: SocketAddr,
    keys_itself: KeysItself,
    notes: Sender<String>,
    stop: Arc<AtomicBool>,
}

impl Session {
    fn note(&self, text: &str) {
        let _ = self.notes.send(format!("VARA: {text}"));
    }

    fn run(self, client: &TcpStream) {
        let vara = match connect_vara(self.ports.vara, &self.stop) {
            Ok(vara) => vara,
            Err(error) => {
                self.note(&format!(
                    "a program ({}) connected, but VARA didn't answer on port {} within {} s \
                     ({error})\n\
                     -> Start VARA (Winlink Express starts it when \"Automatically launch \
                     VARA\" is on), then connect again.",
                    self.peer,
                    self.ports.vara,
                    VARA_STARTS_WITHIN.as_secs()
                ));
                return;
            }
        };
        self.note(&format!("a program connected ({})", self.peer));
        let (Ok(client_reader), Ok(vara_writer)) = (client.try_clone(), vara.try_clone()) else {
            return;
        };
        // What the client says goes to VARA unchanged.
        let stop = Arc::clone(&self.stop);
        let upward = thread::spawn(move || copy(&client_reader, &vara_writer, &stop, |_| {}));
        // What VARA says goes to the client unchanged, after keying or
        // unkeying the radio when VARA asks.
        let mut commands = Commands::default();
        let mut keyer = Keyer::new(&self);
        if let Err(error) = keyer.connect() {
            self.note(&format!("the Agent's radio server didn't answer ({error})"));
        }
        copy(&vara, client, &self.stop, |chunk| {
            for ptt in commands.feed(chunk) {
                keyer.ptt(ptt);
            }
        });
        let _ = upward.join();
        keyer.done();
        self.note(&format!("a program disconnected ({})", self.peer));
    }
}

/// Keys the radio for one client of VARA's, through the Agent's `rigctld`
/// server.
struct Keyer<'a> {
    session: &'a Session,
    /// Whether the Agent keys for this client: decided at the first `PTT ON`.
    agent_keys: Option<bool>,
    rigctld: Option<BufReader<TcpStream>>,
    keyed: bool,
}

impl<'a> Keyer<'a> {
    fn new(session: &'a Session) -> Self {
        Self {
            session,
            agent_keys: None,
            rigctld: None,
            keyed: false,
        }
    }

    fn ptt(&mut self, ptt: Ptt) {
        let agent_keys = *self.agent_keys.get_or_insert_with(|| {
            let proxy = SocketAddr::from(([127, 0, 0, 1], self.session.ports.clients));
            let itself = (self.session.keys_itself)(
                self.session.peer,
                proxy,
                self.session.ports.rigctld.port(),
            );
            self.session.note(if itself {
                "its client keys the radio through the Agent itself, so the Agent leaves \
                 VARA's PTT requests to it"
            } else {
                "the Agent keys the radio when VARA asks (PTT ON), and unkeys it when VARA is \
                 done or the program goes"
            });
            !itself
        });
        if !agent_keys {
            return;
        }
        let on = ptt == Ptt::On;
        if !on && !self.keyed {
            return;
        }
        match self.ask(if on { "T 1" } else { "T 0" }) {
            Ok(answer) if answer == "RPRT 0" => self.keyed = on,
            Ok(answer) => {
                self.session.note(&format!(
                    "the Agent couldn't {} the radio ({answer})",
                    if on { "key" } else { "unkey" }
                ));
                if !on {
                    // The server unkeys a program that goes.
                    self.rigctld = None;
                    self.keyed = false;
                }
            }
            Err(error) => {
                self.session
                    .note(&format!("the Agent's radio server didn't answer ({error})"));
                self.rigctld = None;
                self.keyed = false;
            }
        }
    }

    /// Connects to the Agent's `rigctld` server, if not connected. Done as
    /// the client connects, so the lab's VARA profile goes on the radio
    /// before VARA first asks to transmit (see `station`'s `Programs`).
    fn connect(&mut self) -> Result<(), String> {
        if self.rigctld.is_none() {
            let stream = TcpStream::connect_timeout(&self.session.ports.rigctld, ANSWER_WITHIN)
                .map_err(|error| error.to_string())?;
            stream
                .set_read_timeout(Some(ANSWER_WITHIN))
                .map_err(|error| error.to_string())?;
            let _ = stream.set_nodelay(true);
            self.rigctld = Some(BufReader::new(stream));
        }
        Ok(())
    }

    /// Sends `line` to the Agent's `rigctld` server and returns its answer.
    fn ask(&mut self, line: &str) -> Result<String, String> {
        self.connect()?;
        let Some(reader) = self.rigctld.as_mut() else {
            return Err("not connected".to_owned());
        };
        reader
            .get_mut()
            .write_all(format!("{line}\n").as_bytes())
            .map_err(|error| error.to_string())?;
        let mut answer = String::new();
        reader
            .read_line(&mut answer)
            .map_err(|error| error.to_string())?;
        Ok(answer.trim().to_owned())
    }

    /// The client has gone: unkey, and let go of the server, which unkeys a
    /// program that goes while keyed in any case.
    fn done(&mut self) {
        if self.keyed {
            let _ = self.ask("T 0");
        }
        self.rigctld = None;
        self.keyed = false;
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::mpsc::{self, Receiver};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{Commands, Ports, Proxy, Ptt};

    #[test]
    fn finds_ptt_requests_across_chunks() {
        let mut commands = Commands::default();
        assert_eq!(commands.feed(b"BUSY OFF\rPTT O"), Vec::<Ptt>::new());
        assert_eq!(
            commands.feed(b"N\rBUFFER 10\rPTT OFF\r"),
            vec![Ptt::On, Ptt::Off]
        );
        assert_eq!(
            commands.feed(b"PTT ON\r\nPTT OFF\n"),
            vec![Ptt::On, Ptt::Off]
        );
        // Only whole commands: not a word inside another.
        assert_eq!(commands.feed(b"XPTT ON\rPTT ONE\r"), Vec::<Ptt>::new());
        // A line too long to be a command is ignored to its end.
        let mut long = vec![b'A'; 5000];
        long.extend_from_slice(b"PTT ON\rPTT ON\r");
        assert_eq!(commands.feed(&long), vec![Ptt::On]);
    }

    /// A pretend `rigctld` server: answers `RPRT 0` and records each line,
    /// and each disconnect as `gone`.
    fn pretend_rigctld() -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| unreachable!("{e}"));
        let address = listener
            .local_addr()
            .unwrap_or_else(|e| unreachable!("{e}"));
        let heard = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&heard);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let log = Arc::clone(&log);
                thread::spawn(move || {
                    let mut writer = stream.try_clone().unwrap_or_else(|e| unreachable!("{e}"));
                    for line in BufReader::new(stream).lines() {
                        let Ok(line) = line else { break };
                        log.lock()
                            .unwrap_or_else(|e| unreachable!("{e}"))
                            .push(line);
                        let _ = writer.write_all(b"RPRT 0\n");
                    }
                    log.lock()
                        .unwrap_or_else(|e| unreachable!("{e}"))
                        .push("gone".to_owned());
                });
            }
        });
        (address, heard)
    }

    /// Listeners on two ports in a row on this computer: a command port and
    /// the data port above it. Held, so a test running alongside can't take
    /// them.
    fn bind_pair() -> (TcpListener, TcpListener) {
        for _ in 0..200 {
            let Ok(first) = TcpListener::bind("127.0.0.1:0") else {
                continue;
            };
            let Ok(address) = first.local_addr() else {
                continue;
            };
            let Some(next) = address.port().checked_add(1) else {
                continue;
            };
            if let Ok(second) = TcpListener::bind(("127.0.0.1", next)) {
                return (first, second);
            }
        }
        unreachable!("no two free ports in a row")
    }

    fn wait_for(heard: &Arc<Mutex<Vec<String>>>, wanted: &[&str]) -> Vec<String> {
        let since = Instant::now();
        loop {
            let now = heard.lock().unwrap_or_else(|e| unreachable!("{e}")).clone();
            if now == wanted || since.elapsed() > Duration::from_secs(5) {
                return now;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    struct Bench {
        _proxy: Proxy,
        clients: u16,
        vara: TcpListener,
        vara_data: TcpListener,
        rigctld: Arc<Mutex<Vec<String>>>,
        _notes: Receiver<String>,
    }

    fn bench(keys_itself: bool) -> Bench {
        let (rigctld, heard) = pretend_rigctld();
        let (vara, vara_data) = bind_pair();
        let vara_port = vara.local_addr().map_or(0, |address| address.port());
        let (notes, heard_notes) = mpsc::channel();
        // The proxy's ports: two in a row that are free, tried until the
        // proxy gets them (a test alongside may take them first).
        for _ in 0..200 {
            let (first, second) = bind_pair();
            let clients = first.local_addr().map_or(0, |address| address.port());
            drop((first, second));
            if let Ok(proxy) = Proxy::start(
                &Ports {
                    clients,
                    vara: vara_port,
                    rigctld,
                },
                Arc::new(move |_, _, _| keys_itself),
                notes.clone(),
            ) {
                return Bench {
                    _proxy: proxy,
                    clients,
                    vara,
                    vara_data,
                    rigctld: heard,
                    _notes: heard_notes,
                };
            }
        }
        unreachable!("the proxy found no two free ports in a row")
    }

    fn read_exactly(stream: &mut TcpStream, count: usize) -> Vec<u8> {
        let mut got = vec![0u8; count];
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap_or_else(|e| unreachable!("{e}"));
        stream
            .read_exact(&mut got)
            .unwrap_or_else(|e| unreachable!("{e}"));
        got
    }

    #[test]
    fn passes_everything_through_and_keys_when_vara_asks() {
        let bench = bench(false);
        let mut client = TcpStream::connect(("127.0.0.1", bench.clients))
            .unwrap_or_else(|e| unreachable!("{e}"));
        let (mut vara, _) = bench.vara.accept().unwrap_or_else(|e| unreachable!("{e}"));
        // The client's commands reach VARA unchanged.
        client
            .write_all(b"MYCALL W0CY\r")
            .unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(read_exactly(&mut vara, 12), b"MYCALL W0CY\r");
        // VARA's reach the client unchanged, and PTT ON keys the radio.
        vara.write_all(b"OK\rPTT ON\r")
            .unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(read_exactly(&mut client, 10), b"OK\rPTT ON\r");
        assert_eq!(wait_for(&bench.rigctld, &["T 1"]), ["T 1"]);
        vara.write_all(b"PTT OFF\r")
            .unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(read_exactly(&mut client, 8), b"PTT OFF\r");
        assert_eq!(wait_for(&bench.rigctld, &["T 1", "T 0"]), ["T 1", "T 0"]);
        // The data port is passed through too, both ways.
        let mut data = TcpStream::connect(("127.0.0.1", bench.clients + 1))
            .unwrap_or_else(|e| unreachable!("{e}"));
        let (mut vara_data, _) = bench
            .vara_data
            .accept()
            .unwrap_or_else(|e| unreachable!("{e}"));
        data.write_all(&[0, 1, 2, 255])
            .unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(read_exactly(&mut vara_data, 4), [0, 1, 2, 255]);
        vara_data
            .write_all(b"hello")
            .unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(read_exactly(&mut data, 5), b"hello");
    }

    #[test]
    fn waits_for_vara_to_start_instead_of_dropping_the_client() {
        let (rigctld, _) = pretend_rigctld();
        // VARA isn't listening yet: its ports are free, and taken a moment
        // after the client connects.
        let (first, second) = bind_pair();
        let vara_port = first.local_addr().map_or(0, |address| address.port());
        drop((first, second));
        let (notes, _heard) = mpsc::channel();
        let mut started = None;
        for _ in 0..200 {
            let (a, b) = bind_pair();
            let clients = a.local_addr().map_or(0, |address| address.port());
            drop((a, b));
            if let Ok(proxy) = Proxy::start(
                &Ports {
                    clients,
                    vara: vara_port,
                    rigctld,
                },
                Arc::new(|_, _, _| false),
                notes.clone(),
            ) {
                started = Some((proxy, clients));
                break;
            }
        }
        let Some((_proxy, clients)) = started else {
            unreachable!("the proxy found no ports")
        };
        let mut client =
            TcpStream::connect(("127.0.0.1", clients)).unwrap_or_else(|e| unreachable!("{e}"));
        thread::sleep(Duration::from_millis(700));
        let vara =
            TcpListener::bind(("127.0.0.1", vara_port)).unwrap_or_else(|e| unreachable!("{e}"));
        let (mut vara, _) = vara.accept().unwrap_or_else(|e| unreachable!("{e}"));
        client
            .write_all(b"VERSION\r")
            .unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(read_exactly(&mut vara, 8), b"VERSION\r");
    }

    #[test]
    fn unkeys_when_the_client_goes_while_keyed() {
        let bench = bench(false);
        let client = TcpStream::connect(("127.0.0.1", bench.clients))
            .unwrap_or_else(|e| unreachable!("{e}"));
        let (mut vara, _) = bench.vara.accept().unwrap_or_else(|e| unreachable!("{e}"));
        vara.write_all(b"PTT ON\r")
            .unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(wait_for(&bench.rigctld, &["T 1"]), ["T 1"]);
        drop(client);
        assert_eq!(
            wait_for(&bench.rigctld, &["T 1", "T 0", "gone"]),
            ["T 1", "T 0", "gone"]
        );
    }

    #[test]
    fn leaves_keying_to_a_client_that_keys_itself() {
        let bench = bench(true);
        let mut client = TcpStream::connect(("127.0.0.1", bench.clients))
            .unwrap_or_else(|e| unreachable!("{e}"));
        let (mut vara, _) = bench.vara.accept().unwrap_or_else(|e| unreachable!("{e}"));
        vara.write_all(b"PTT ON\rPTT OFF\r")
            .unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(read_exactly(&mut client, 15), b"PTT ON\rPTT OFF\r");
        thread::sleep(Duration::from_millis(200));
        assert!(
            bench
                .rigctld
                .lock()
                .unwrap_or_else(|e| unreachable!("{e}"))
                .is_empty()
        );
    }
}
