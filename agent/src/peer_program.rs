// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Which program on this computer is at the other end of a connection.
//!
//! Read from Windows' own `netstat -ano -p TCP`, which lists each TCP
//! connection as protocol, local address, foreign address, state and the
//! owning process id (Microsoft Learn, "netstat": `-a` all connections,
//! `-n` numbers, `-o` owning process id, `-p` protocol). The state column is
//! not read, because Windows translates it.

use std::net::SocketAddr;

/// One row of `netstat -ano`: a connection from `local` to `foreign`, owned
/// by process `pid`.
#[derive(Debug, PartialEq, Eq)]
struct Row {
    local: SocketAddr,
    foreign: SocketAddr,
    pid: u32,
}

/// The TCP connections listed in `netstat -ano` output.
fn rows(table: &str) -> Vec<Row> {
    table
        .lines()
        .filter_map(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            match words.as_slice() {
                [protocol, local, foreign, .., pid] if protocol.eq_ignore_ascii_case("TCP") => {
                    Some(Row {
                        local: local.parse().ok()?,
                        foreign: foreign.parse().ok()?,
                        pid: pid.parse().ok()?,
                    })
                }
                _ => None,
            }
        })
        .collect()
}

/// Whether the program that connected from `client` (as seen by a server on
/// this computer) also has a connection open to `port` on this computer,
/// according to the `netstat -ano` output `table`.
#[must_use]
pub fn also_connected_to(table: &str, client: SocketAddr, port: u16) -> bool {
    let rows = rows(table);
    let Some(owner) = rows
        .iter()
        .find(|row| row.local == client)
        .map(|row| row.pid)
    else {
        return false;
    };
    rows.iter()
        .any(|row| row.pid == owner && row.foreign.ip().is_loopback() && row.foreign.port() == port)
}

/// Whether the program connected from `client` already keys the radio
/// itself, through the Agent's `rigctld` server on `rigctld_port`: it has a
/// connection open to it. Unknown (the list can't be read) counts as no.
#[cfg(windows)]
#[must_use]
pub fn keys_itself(client: SocketAddr, rigctld_port: u16) -> bool {
    std::process::Command::new("netstat")
        .args(["-ano", "-p", "TCP"])
        .output()
        .is_ok_and(|listed| {
            also_connected_to(
                &String::from_utf8_lossy(&listed.stdout),
                client,
                rigctld_port,
            )
        })
}

#[cfg(test)]
mod tests {
    use super::also_connected_to;

    const TABLE: &str = "\r
Active Connections\r
\r
  Proto  Local Address          Foreign Address        State           PID\r
  TCP    0.0.0.0:135            0.0.0.0:0              LISTENING       1012\r
  TCP    127.0.0.1:4532         0.0.0.0:0              LISTENING       7000\r
  TCP    127.0.0.1:4532         127.0.0.1:50410        ESTABLISHED     7000\r
  TCP    127.0.0.1:8300         0.0.0.0:0              LISTENING       7000\r
  TCP    127.0.0.1:8300         127.0.0.1:50402        ESTABLISHED     7000\r
  TCP    127.0.0.1:8300         127.0.0.1:50500        ESTABLISHED     7000\r
  TCP    127.0.0.1:50402        127.0.0.1:8300         HERGESTELLT     4100\r
  TCP    127.0.0.1:50410        127.0.0.1:4532         ESTABLISHED     4100\r
  TCP    127.0.0.1:50500        127.0.0.1:8300         ESTABLISHED     5200\r
  TCP    192.168.1.20:50600     52.1.2.3:443           ESTABLISHED     5200\r
";

    #[test]
    fn finds_a_client_that_also_uses_the_radio_server() {
        // Process 4100 (say, VarAC) uses both VARA and the radio server.
        assert!(also_connected_to(
            TABLE,
            "127.0.0.1:50402"
                .parse()
                .unwrap_or_else(|e| unreachable!("{e}")),
            4532
        ));
        // Process 5200 (say, Winlink Express) only VARA.
        assert!(!also_connected_to(
            TABLE,
            "127.0.0.1:50500"
                .parse()
                .unwrap_or_else(|e| unreachable!("{e}")),
            4532
        ));
        // A connection not in the list.
        assert!(!also_connected_to(
            TABLE,
            "127.0.0.1:59999"
                .parse()
                .unwrap_or_else(|e| unreachable!("{e}")),
            4532
        ));
        assert!(!also_connected_to(
            "",
            "127.0.0.1:50402"
                .parse()
                .unwrap_or_else(|e| unreachable!("{e}")),
            4532
        ));
    }
}
