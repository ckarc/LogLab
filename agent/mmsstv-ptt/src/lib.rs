// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! LabLog's PTT plugin for MMSSTV: keys the radio through the Station Agent.
//!
//! MMSSTV keys a radio through the port named by `PTT=` in `[Define]` of
//! `Mmsstv.ini`. When that name isn't a port Windows can open, MMSSTV loads
//! `NAME.fsk` (with Windows' usual DLL search, its own folder first) and
//! calls five functions in it. This is what MMSSTV's source shows
//! (github.com/n5ac/mmsstv @ 8060b5f):
//!
//! - `Comm.cpp`, `CComm::Open`: `CreateFile` on the name, then on
//!   `\\.\NAME`; if both fail, `new CEXTFSK(PortName)`.
//! - `Comm.cpp`, `CEXTFSK::CEXTFSK`: `LoadLibrary("NAME.fsk")` (`EXTFSK.dll`
//!   for the name `EXTFSK`), and `GetProcAddress` of `extfskOpen`,
//!   `extfskClose`, `extfskIsTxBusy`, `extfskPutChar` and `extfskSetPTT`,
//!   each tried without and then with a leading underscore.
//! - `Comm.h`: all five are `__stdcall`: `long Open(long)`, `void Close()`,
//!   `long IsTxBusy()`, `void PutChar(BYTE)`, `void SetPTT(long)`.
//! - `Comm.cpp`, `CComm::SetPTT`: bit 0 of `SetPTT`'s value is the PTT;
//!   bit 1 is the "RTS on receive" scan signal, which this ignores.
//! - `Main.cpp`, `TMmsstv::ToTX`: `SetPTT(1)`, then 10 ms, then the sound
//!   starts. So `SetPTT` here waits for the Agent's answer before returning,
//!   and the picture's first tones go out with the radio already keyed.
//!
//! Keying is asked of the Agent with the Hamlib `rigctld` command `T 1` (and
//! `T 0`) on `127.0.0.1:4532`, the address the Agent serves on by default,
//! the same way WSJT-X keys through it. It first says it is MMSSTV
//! (`\lablog_program MMSSTV`, the Agent's own command), for the longer
//! time limit a picture needs. The plugin keeps that connection
//! open; if MMSSTV closes or stops, the connection drops and the Agent
//! unkeys the radio itself (`rigctld_server`: "a program that disconnects
//! while keyed is unkeyed"). The plugin never touches radio hardware
//! (Principle IX).
//!
//! The interface was read from MMSSTV's published source (LGPL-3.0); no
//! code is taken from it (Principle XVI).

use std::io::{self, BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

/// Where the Agent serves the radio: its default `--listen` address.
const AGENT: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 4532);

/// The longest the plugin waits to reach the Agent, or for its answer.
/// MMSSTV waits too, on its window's thread.
const WAIT: Duration = Duration::from_secs(2);

/// LabLog's own command naming the program (the Agent's `rigctld_server`).
const NAMED: &str = "\\lablog_program MMSSTV";

/// Hamlib's "it worked" answer.
const DONE: &str = "RPRT 0";

/// The one connection to the Agent, while there is one.
static LINK: Mutex<Option<Link>> = Mutex::new(None);

/// A connection to the Agent.
#[derive(Debug)]
pub struct Link {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

impl Link {
    /// Reaches the Agent at `at`, and says this is MMSSTV, whose pictures
    /// take longer to send than the Agent lets other programs transmit at
    /// once.
    ///
    /// # Errors
    ///
    /// The Agent isn't there, or didn't answer in time.
    pub fn connect(at: SocketAddr) -> io::Result<Self> {
        let writer = TcpStream::connect_timeout(&at, WAIT)?;
        writer.set_read_timeout(Some(WAIT))?;
        writer.set_write_timeout(Some(WAIT))?;
        writer.set_nodelay(true)?;
        let reader = BufReader::new(writer.try_clone()?);
        let mut link = Self { reader, writer };
        // Any answer will do: an older Agent doesn't know the command.
        link.ask(NAMED)?;
        Ok(link)
    }

    /// Sends `command` and returns the Agent's one-line answer.
    fn ask(&mut self, command: &str) -> io::Result<String> {
        self.writer.write_all(format!("{command}\n").as_bytes())?;
        let mut answer = String::new();
        if self.reader.read_line(&mut answer)? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        Ok(answer.trim().to_owned())
    }
}

/// `slot`, even if a thread panicked while holding it (nothing it holds can
/// be left half-changed).
fn hold(slot: &Mutex<Option<Link>>) -> MutexGuard<'_, Option<Link>> {
    slot.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Connects `slot` to the Agent at `at`, if it isn't already. Whether it is
/// connected now.
pub fn open(slot: &Mutex<Option<Link>>, at: SocketAddr) -> bool {
    let mut link = hold(slot);
    if link.is_none() {
        *link = Link::connect(at).ok();
    }
    link.is_some()
}

/// Asks the Agent at `at` to key the radio (`on`) or unkey it, through
/// `slot`'s connection, connecting (once more, if a kept connection has
/// gone) as needed. Whether the Agent said it was done.
///
/// Unkeying with no connection is done already: nothing was keyed through
/// one, or the Agent unkeyed when it dropped.
pub fn key(slot: &Mutex<Option<Link>>, at: SocketAddr, on: bool) -> bool {
    let command = if on { "T 1" } else { "T 0" };
    let mut link = hold(slot);
    for _ in 0..2 {
        if link.is_none() {
            if !on {
                return true;
            }
            match Link::connect(at) {
                Ok(fresh) => *link = Some(fresh),
                Err(_) => return false,
            }
        }
        let Some(open) = link.as_mut() else {
            return false;
        };
        match open.ask(command) {
            Ok(answer) => return answer == DONE,
            // A kept connection the Agent has since closed (it restarted):
            // drop it and try once on a new one.
            Err(_) => *link = None,
        }
    }
    !on
}

/// Whether MMSSTV's `SetPTT` value `tx` asks for the radio keyed: bit 0.
/// Bit 1 is its "RTS on receive" scan signal, not a PTT.
#[must_use]
pub fn keyed(tx: i32) -> bool {
    tx & 1 == 1
}

/// Unkeys the radio if this connection keyed it, then drops the connection.
pub fn close(slot: &Mutex<Option<Link>>) {
    let mut link = hold(slot);
    if let Some(open) = link.as_mut() {
        let _ = open.ask("T 0");
    }
    *link = None;
}

/// The functions MMSSTV calls, by the names and calling convention its
/// source gives (see the crate's notes).
///
/// `unsafe_code` is allowed here, and only here, for `#[unsafe(no_mangle)]`:
/// MMSSTV finds these by name with `GetProcAddress`, so their names must be
/// exported exactly as written. That is the platform interface this plugin
/// exists for; no `unsafe` block is used.
#[allow(unsafe_code, non_snake_case)]
mod exports {
    use super::{AGENT, LINK, close, key, keyed, open};

    /// MMSSTV opens the "port". `para` gives a teleprinter's speed and
    /// shape, which a PTT has no use for. Always succeeds: MMSSTV ignores
    /// the answer, and a missing Agent is found when keying.
    #[unsafe(no_mangle)]
    pub extern "system" fn extfskOpen(_para: i32) -> i32 {
        let _ = open(&LINK, AGENT);
        1
    }

    /// MMSSTV closes the "port": unkey, and let go of the Agent.
    #[unsafe(no_mangle)]
    pub extern "system" fn extfskClose() {
        close(&LINK);
    }

    /// Never busy: no characters are sent.
    #[unsafe(no_mangle)]
    pub extern "system" fn extfskIsTxBusy() -> i32 {
        0
    }

    /// Teleprinter characters; MMSSTV sends none for a PTT, and none are
    /// sent on.
    #[unsafe(no_mangle)]
    pub extern "system" fn extfskPutChar(_character: u8) {}

    /// Keys (bit 0 set) or unkeys the radio through the Agent.
    #[unsafe(no_mangle)]
    pub extern "system" fn extfskSetPTT(tx: i32) {
        let _ = key(&LINK, AGENT, keyed(tx));
    }
}

#[cfg(test)]
mod tests {
    use super::{Link, close, key, keyed, open};
    use std::io::{BufRead, BufReader, Write};
    use std::net::{SocketAddr, TcpListener};
    use std::sync::Mutex;
    use std::thread;

    const HELLO: &str = "\\lablog_program MMSSTV";

    /// A pretend Agent: answers each line with `answer`, records the lines,
    /// and serves `connections` connections, one after another.
    fn agent(
        answer: &'static str,
        connections: usize,
    ) -> (SocketAddr, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap_or_else(|why| unreachable!("{why}"));
        let at = listener
            .local_addr()
            .unwrap_or_else(|why| unreachable!("{why}"));
        let heard = thread::spawn(move || {
            let mut heard = Vec::new();
            for _ in 0..connections {
                let Ok((stream, _)) = listener.accept() else {
                    break;
                };
                let mut writer = stream
                    .try_clone()
                    .unwrap_or_else(|why| unreachable!("{why}"));
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { break };
                    heard.push(line);
                    if writer.write_all(format!("{answer}\n").as_bytes()).is_err() {
                        break;
                    }
                }
                heard.push("(closed)".to_owned());
            }
            heard
        });
        (at, heard)
    }

    /// An address nothing listens on.
    fn nobody() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap_or_else(|why| unreachable!("{why}"));
        listener
            .local_addr()
            .unwrap_or_else(|why| unreachable!("{why}"))
    }

    #[test]
    fn keys_and_unkeys_on_one_connection_and_unkeys_on_close() {
        let (at, heard) = agent("RPRT 0", 1);
        let slot = Mutex::new(None);
        assert!(open(&slot, at));
        assert!(key(&slot, at, true));
        assert!(key(&slot, at, false));
        assert!(key(&slot, at, true));
        close(&slot);
        let heard = heard.join().unwrap_or_else(|_| unreachable!());
        assert_eq!(heard, [HELLO, "T 1", "T 0", "T 1", "T 0", "(closed)"]);
    }

    #[test]
    fn says_so_when_the_agent_refuses() {
        let (at, heard) = agent("RPRT -9", 1);
        let slot = Mutex::new(None);
        assert!(!key(&slot, at, true));
        close(&slot);
        drop(heard.join());
    }

    #[test]
    fn with_no_agent_keying_fails_and_unkeying_is_already_done() {
        let at = nobody();
        let slot: Mutex<Option<Link>> = Mutex::new(None);
        assert!(!open(&slot, at));
        assert!(!key(&slot, at, true));
        assert!(key(&slot, at, false));
        close(&slot);
    }

    #[test]
    fn reconnects_once_when_the_agent_restarted() {
        let (at, heard) = agent("RPRT 0", 2);
        let slot = Mutex::new(None);
        assert!(open(&slot, at));
        // The Agent drops the connection (as when it restarts).
        if let Some(link) = slot.lock().unwrap_or_else(|_| unreachable!()).as_mut() {
            let _ = link.writer.shutdown(std::net::Shutdown::Both);
        }
        assert!(key(&slot, at, true));
        close(&slot);
        let heard = heard.join().unwrap_or_else(|_| unreachable!());
        assert_eq!(heard, [HELLO, "(closed)", HELLO, "T 1", "T 0", "(closed)"]);
    }

    #[test]
    fn only_bit_zero_keys() {
        assert!(!keyed(0));
        assert!(keyed(1));
        assert!(!keyed(2));
        assert!(keyed(3));
    }
}
