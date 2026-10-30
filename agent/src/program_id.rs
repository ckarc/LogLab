// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Which program connected to the virtual radio, so the lab's profile for
//! it can go on as it starts.
//!
//! A program's connection to the Agent is a TCP connection on this
//! computer, and Windows says which process owns each end of every TCP
//! connection: `GetExtendedTcpTable` with `TCP_TABLE_OWNER_PID_ALL` gives a
//! `MIB_TCPTABLE_OWNER_PID`, a count and then one `MIB_TCPROW_OWNER_PID`
//! per connection: state, local address, local port, remote address,
//! remote port and owning process id, each a 32-bit word, addresses and
//! ports in network byte order, a port in the low 16 bits of its word
//! (Microsoft Learn: "`GetExtendedTcpTable` function (iphlpapi.h)",
//! "`MIB_TCPROW_OWNER_PID` structure (tcpmib.h)"). The program's own end
//! is the row whose local end is where the program connected from and
//! whose remote port is the Agent's. Its process's executable then names
//! the program (`QueryFullProcessImageNameW`).
//!
//! `VarAC`, not VARA, is the program that connects: VARA leaves the radio
//! to `VarAC`, so `VarAC` starting means the VARA profile.

// The table is read on Windows only, and tested everywhere.
#![cfg_attr(not(windows), allow(dead_code))]

use std::net::{Ipv4Addr, SocketAddr};

/// One row of the table: a connection's ends and the process that owns
/// it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    /// The owner's end.
    pub local: (Ipv4Addr, u16),
    /// The other end.
    pub remote: (Ipv4Addr, u16),
    /// The owning process.
    pub pid: u32,
}

/// The words in one `MIB_TCPROW_OWNER_PID`.
const ROW_WORDS: usize = 6;

/// Reads a `MIB_TCPTABLE_OWNER_PID` as Windows fills it in: rows past the
/// end of `table` are left out.
#[must_use]
pub fn rows(table: &[u8]) -> Vec<Row> {
    let word = |at: usize| -> Option<[u8; 4]> { table.get(at..at + 4)?.try_into().ok() };
    let Some(count) = word(0).map(u32::from_ne_bytes) else {
        return Vec::new();
    };
    let address = |w: [u8; 4]| Ipv4Addr::new(w[0], w[1], w[2], w[3]);
    let port = |w: [u8; 4]| u16::from_be_bytes([w[0], w[1]]);
    (0..usize::try_from(count).unwrap_or(0))
        .map_while(|n| {
            let at = 4 + n * ROW_WORDS * 4;
            let w = |i: usize| word(at + i * 4);
            Some(Row {
                local: (address(w(1)?), port(w(2)?)),
                remote: (address(w(3)?), port(w(4)?)),
                pid: u32::from_ne_bytes(w(5)?),
            })
        })
        .collect()
}

/// The process that owns the program's end of the connection from
/// `program` to the Agent listening at `server`.
#[must_use]
pub fn owner(rows: &[Row], program: SocketAddr, server: SocketAddr) -> Option<u32> {
    let SocketAddr::V4(program) = program else {
        return None;
    };
    rows.iter()
        .find(|row| row.local == (*program.ip(), program.port()) && row.remote.1 == server.port())
        .map(|row| row.pid)
}

/// Whether the program on the far end of the connection from `program` to
/// the Agent listening at `server` also has a connection open to `port` on
/// this computer: for VARA's client, whether it keys the radio through the
/// Agent's radio server itself (see `vara_proxy`).
#[must_use]
pub fn also_connected(rows: &[Row], program: SocketAddr, server: SocketAddr, port: u16) -> bool {
    owner(rows, program, server).is_some_and(|pid| {
        rows.iter()
            .any(|row| row.pid == pid && row.remote.0.is_loopback() && row.remote.1 == port)
    })
}

/// Whether the program that connected from `program` to the Agent's VARA
/// proxy at `server` keys the radio through the Agent's radio server, on
/// `rigctld_port`, itself. Unknown (Windows won't say) counts as no.
#[cfg(windows)]
#[must_use]
pub fn keys_itself(program: SocketAddr, server: SocketAddr, rigctld_port: u16) -> bool {
    windows_api::tcp_table()
        .is_ok_and(|table| also_connected(&rows(&table), program, server, rigctld_port))
}

/// The program setup for a process, by its executable's name or path:
/// `VarAC` means VARA. Case doesn't matter, as on Windows.
#[must_use]
pub fn setup_for(executable: &str) -> Option<&'static str> {
    let name = executable
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(executable)
        .to_ascii_lowercase();
    let stem = name.strip_suffix(".exe").unwrap_or(&name);
    // The Agent itself connects to its radio server only from its VARA
    // proxy, keying for VARA's client (Winlink Express): VARA.
    if stem.starts_with("varac") || stem == "vara" || stem == "varahf" || stem == "loglab-agent" {
        Some("VARA")
    } else if stem == "wsjtx" {
        Some("FT8")
    } else if stem == "js8call" {
        Some("JS8")
    } else if stem == "fldigi" {
        Some("fldigi")
    } else if stem == "mmsstv" {
        Some("SSTV")
    } else {
        None
    }
}

/// The process id and executable of the program that connected from
/// `program` to the Agent listening at `server`.
///
/// # Errors
///
/// Windows won't list its connections, or the program's end isn't among
/// them, or its executable can't be read.
#[cfg(windows)]
pub fn identify(program: SocketAddr, server: SocketAddr) -> Result<(u32, String), String> {
    let table = windows_api::tcp_table()?;
    let pid = owner(&rows(&table), program, server)
        .ok_or_else(|| format!("no process owns the connection from {program}"))?;
    Ok((pid, windows_api::executable(pid)?))
}

/// Which program connected can be told on Windows only.
///
/// # Errors
///
/// Always, off Windows.
#[cfg(not(windows))]
pub fn identify(program: SocketAddr, server: SocketAddr) -> Result<(u32, String), String> {
    let _ = (program, server);
    Err("which program connected can be told on Windows only".to_owned())
}

/// The two Windows calls, the only unsafe code here.
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows_api {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, TCP_TABLE_OWNER_PID_ALL,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::core::PWSTR;

    /// `AF_INET` (ws2def.h): IPv4. The Agent listens on 127.0.0.1.
    const AF_INET: u32 = 2;
    /// `NO_ERROR` (winerror.h).
    const NO_ERROR: u32 = 0;
    /// `ERROR_INSUFFICIENT_BUFFER` (winerror.h): the table grew; ask again.
    const ERROR_INSUFFICIENT_BUFFER: u32 = 122;

    /// Every IPv4 TCP connection on this computer and its owner, as
    /// `GetExtendedTcpTable` writes them.
    pub fn tcp_table() -> Result<Vec<u8>, String> {
        let mut size: u32 = 0;
        for _ in 0..4 {
            // u32 words, so the table is aligned as Windows expects.
            let mut table = vec![0_u32; usize::try_from(size).unwrap_or(0).div_ceil(4)];
            let pointer = (!table.is_empty()).then(|| table.as_mut_ptr().cast());
            // SAFETY: `pointer` is None or points at `table`, which has room
            // for at least `size` bytes, the size passed; Windows writes no
            // more than that, and sets `size` to what it needs when that is
            // too small (Microsoft Learn: GetExtendedTcpTable).
            let result = unsafe {
                GetExtendedTcpTable(
                    pointer,
                    &raw mut size,
                    false,
                    AF_INET,
                    TCP_TABLE_OWNER_PID_ALL,
                    0,
                )
            };
            match result {
                NO_ERROR if !table.is_empty() => {
                    return Ok(table.iter().flat_map(|word| word.to_ne_bytes()).collect());
                }
                NO_ERROR | ERROR_INSUFFICIENT_BUFFER => {
                    size = size.saturating_add(1024);
                }
                error => {
                    return Err(format!(
                        "Windows wouldn't list its connections (error {error})"
                    ));
                }
            }
        }
        Err("Windows' list of connections kept growing".to_owned())
    }

    /// The full path of process `pid`'s executable.
    pub fn executable(pid: u32) -> Result<String, String> {
        // SAFETY: OpenProcess takes plain values and returns a handle or an
        // error; the handle is closed below.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
            .map_err(|error| format!("can't look at process {pid}: {error}"))?;
        let mut name = vec![0_u16; 1024];
        let mut size = u32::try_from(name.len()).unwrap_or(0);
        // SAFETY: `name` has room for `size` UTF-16 units, the size passed;
        // Windows writes at most that and sets `size` to what it wrote.
        let read = unsafe {
            QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                PWSTR(name.as_mut_ptr()),
                &raw mut size,
            )
        };
        // SAFETY: `process` is the open handle from OpenProcess, closed
        // once.
        let _ = unsafe { CloseHandle(process) };
        read.map_err(|error| format!("can't read process {pid}'s executable: {error}"))?;
        name.truncate(usize::try_from(size).unwrap_or(0));
        Ok(String::from_utf16_lossy(&name))
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr};

    use super::{Row, also_connected, owner, rows, setup_for};

    /// One row as Windows writes it: words in this computer's order,
    /// addresses and ports in network order.
    fn row(local: (Ipv4Addr, u16), remote: (Ipv4Addr, u16), pid: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&5_u32.to_ne_bytes()); // MIB_TCP_STATE_ESTAB
        for (address, port) in [local, remote] {
            bytes.extend_from_slice(&address.octets());
            let port = port.to_be_bytes();
            bytes.extend_from_slice(&[port[0], port[1], 0, 0]);
        }
        bytes.extend_from_slice(&pid.to_ne_bytes());
        bytes
    }

    #[test]
    fn finds_the_process_on_the_programs_end_of_the_connection() {
        let home = Ipv4Addr::LOCALHOST;
        let mut table = 3_u32.to_ne_bytes().to_vec();
        // The Agent's own end, the program's end, and another program.
        table.extend(row((home, 4532), (home, 50123), 100));
        table.extend(row((home, 50123), (home, 4532), 4242));
        table.extend(row((home, 50200), (home, 8300), 7));
        let read = rows(&table);
        assert_eq!(read.len(), 3);
        assert_eq!(
            read[1],
            Row {
                local: (home, 50123),
                remote: (home, 4532),
                pid: 4242
            }
        );
        let server: SocketAddr = "127.0.0.1:4532".parse().unwrap_or_else(|_| unreachable!());
        let program: SocketAddr = "127.0.0.1:50123".parse().unwrap_or_else(|_| unreachable!());
        assert_eq!(owner(&read, program, server), Some(4242));
        let gone: SocketAddr = "127.0.0.1:50999".parse().unwrap_or_else(|_| unreachable!());
        assert_eq!(owner(&read, gone, server), None);
        // A table cut short keeps only whole rows.
        assert_eq!(rows(&table[..table.len() - 1]).len(), 2);
        assert!(rows(&[]).is_empty());
    }

    #[test]
    fn varac_means_vara() {
        assert_eq!(setup_for(r"C:\VarAC\VarAC.exe"), Some("VARA"));
        assert_eq!(setup_for("varac.EXE"), Some("VARA"));
        assert_eq!(setup_for(r"C:\WSJT\wsjtx\bin\wsjtx.exe"), Some("FT8"));
        assert_eq!(setup_for("js8call.exe"), Some("JS8"));
        assert_eq!(setup_for("fldigi.exe"), Some("fldigi"));
        assert_eq!(setup_for("Mmsstv.exe"), Some("SSTV"));
        assert_eq!(setup_for(r"C:\Program Files\N1MM\N1MMLogger.net.exe"), None);
        // The Agent's own VARA proxy, keying for Winlink Express.
        assert_eq!(setup_for(r"C:\LabLog\loglab-agent.exe"), Some("VARA"));
    }

    #[test]
    fn tells_a_vara_client_that_keys_through_the_radio_server_itself() {
        let home = Ipv4Addr::LOCALHOST;
        let read = vec![
            // VarAC: to VARA's proxy and to the radio server.
            Row {
                local: (home, 50402),
                remote: (home, 8300),
                pid: 4100,
            },
            Row {
                local: (home, 50410),
                remote: (home, 4532),
                pid: 4100,
            },
            // Winlink Express: to VARA's proxy only.
            Row {
                local: (home, 50500),
                remote: (home, 8300),
                pid: 5200,
            },
            // The Agent's own ends.
            Row {
                local: (home, 8300),
                remote: (home, 50402),
                pid: 7000,
            },
            Row {
                local: (home, 4532),
                remote: (home, 50410),
                pid: 7000,
            },
        ];
        let proxy: SocketAddr = "127.0.0.1:8300".parse().unwrap_or_else(|_| unreachable!());
        let at = |port: u16| SocketAddr::from((home, port));
        assert!(also_connected(&read, at(50402), proxy, 4532));
        assert!(!also_connected(&read, at(50500), proxy, 4532));
        assert!(!also_connected(&read, at(59999), proxy, 4532));
    }
}
