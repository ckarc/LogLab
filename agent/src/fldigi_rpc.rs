// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Asking fldigi which modem it is on, so the radio can be set up for CW
//! when fldigi is on CW, and back for the data modes when it leaves it.
//!
//! fldigi answers XML-RPC on this computer: its `--xmlrpc-server-address`
//! "Defaults to "127.0.0.1"" and `--xmlrpc-server-port` "Defaults to
//! "7362"" (fldigi(1) manual, OPTIONS). Its `modem.get_name` "Returns the
//! name of the current modem" (pyFldigi's table of fldigi's XML-RPC
//! commands), "BPSK31", say (fldigi wiki, "xmlrpc howto", XML-RPC shell
//! scripting). A call is an HTTP POST of a `methodCall` document, and the
//! answer a `methodResponse` holding one `value`, or a `fault` (XML-RPC
//! specification, xmlrpc.com/spec: "Request example", "Response example",
//! "Fault example"; a `value` with no type is a string).
//!
//! Only `modem.get_name` is ever called: it reads, and can't transmit.
//! The answer is untrusted: it is read only up to a size, and anything not
//! shaped as an answer is an error (Principle XII).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

/// Where fldigi listens for XML-RPC unless told otherwise.
pub const ADDRESS: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 7362);

/// The most of an answer read: a modem's name is a few bytes.
const MAX_ANSWER: u64 = 16 * 1024;

/// The call that asks for the current modem's name.
const BODY: &str =
    "<?xml version=\"1.0\"?>\r\n<methodCall><methodName>modem.get_name</methodName></methodCall>";

/// The whole HTTP request for `modem.get_name`.
#[must_use]
pub fn request() -> String {
    format!(
        "POST /RPC2 HTTP/1.1\r\nHost: 127.0.0.1\r\nUser-Agent: LabLog\r\nContent-Type: \
         text/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{BODY}",
        BODY.len()
    )
}

/// The modem's name in fldigi's HTTP answer `text`.
///
/// # Errors
///
/// The answer isn't HTTP 200, is a fault, or holds no value.
pub fn name_in(text: &str) -> Result<String, String> {
    let status = text.lines().next().unwrap_or_default();
    if status.split_whitespace().nth(1) != Some("200") {
        return Err(format!("fldigi answered {status:?}"));
    }
    let body = text.split_once("\r\n\r\n").map_or("", |(_, body)| body);
    if body.contains("<fault>") {
        return Err("fldigi answered with a fault".to_owned());
    }
    let inside = between(body, "<value>", "</value>")
        .ok_or_else(|| "fldigi's answer holds no value".to_owned())?;
    let name = between(inside, "<string>", "</string>").unwrap_or(inside);
    if name.contains('<') {
        return Err("fldigi's answer isn't a name".to_owned());
    }
    let name = name
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    Ok(name.trim().to_owned())
}

/// The text between the first `open` and the `close` after it.
fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)? + start;
    Some(&text[start..end])
}

/// Whether fldigi's modem `name` is one of its CW modems.
#[must_use]
pub fn is_cw(name: &str) -> bool {
    name.get(..2)
        .is_some_and(|start| start.eq_ignore_ascii_case("CW"))
}

/// Asks fldigi at `address` for its current modem's name.
///
/// # Errors
///
/// fldigi isn't listening there, didn't answer within `wait`, or answered
/// with something else.
pub fn modem_name(address: SocketAddr, wait: Duration) -> Result<String, String> {
    let mut stream =
        TcpStream::connect_timeout(&address, wait).map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(wait))
        .and_then(|()| stream.set_write_timeout(Some(wait)))
        .map_err(|error| error.to_string())?;
    stream
        .write_all(request().as_bytes())
        .map_err(|error| error.to_string())?;
    let mut answer = Vec::new();
    stream
        .take(MAX_ANSWER)
        .read_to_end(&mut answer)
        .map_err(|error| error.to_string())?;
    name_in(&String::from_utf8_lossy(&answer))
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    use super::{is_cw, modem_name, name_in, request};

    fn answer(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nServer: XMLRPC++ 0.8\r\nContent-Type: text/xml\r\n\
             Content-length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    #[test]
    fn reads_the_modem_name_typed_or_not() {
        let typed = "<?xml version=\"1.0\"?>\r\n<methodResponse><params><param>\r\n\t<value>\
                     <string>BPSK31</string></value>\r\n</param></params></methodResponse>\r\n";
        assert_eq!(name_in(&answer(typed)), Ok("BPSK31".to_owned()));
        let bare = "<methodResponse><params><param><value>CW</value></param></params>\
                    </methodResponse>";
        assert_eq!(name_in(&answer(bare)), Ok("CW".to_owned()));
    }

    #[test]
    fn anything_but_an_answer_is_an_error() {
        let fault = "<methodResponse><fault><value><struct></struct></value></fault>\
                     </methodResponse>";
        assert!(name_in(&answer(fault)).is_err());
        assert!(name_in("HTTP/1.1 404 Not Found\r\n\r\n").is_err());
        assert!(name_in(&answer("<methodResponse></methodResponse>")).is_err());
        assert!(name_in(&answer("<value><int>4</int></value>")).is_err());
        assert!(name_in("").is_err());
    }

    #[test]
    fn only_cw_modems_count_as_cw() {
        assert!(is_cw("CW"));
        assert!(is_cw("cw"));
        assert!(!is_cw("BPSK31"));
        assert!(!is_cw("RTTY"));
        assert!(!is_cw("C"));
        assert!(!is_cw(""));
    }

    #[test]
    fn asks_once_with_a_whole_request_and_reads_the_answer() {
        assert!(request().starts_with("POST /RPC2 HTTP/1.1\r\n"));
        assert!(request().ends_with("<methodName>modem.get_name</methodName></methodCall>"));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| unreachable!("{e}"));
        let address = listener
            .local_addr()
            .unwrap_or_else(|e| unreachable!("{e}"));
        let fldigi = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap_or_else(|e| unreachable!("{e}"));
            let mut asked = vec![0; request().len()];
            stream
                .read_exact(&mut asked)
                .unwrap_or_else(|e| unreachable!("{e}"));
            let _ = stream.write_all(answer("<value>CW</value>").as_bytes());
            String::from_utf8_lossy(&asked).into_owned()
        });
        assert_eq!(
            modem_name(address, Duration::from_secs(2)),
            Ok("CW".to_owned())
        );
        assert_eq!(fldigi.join().unwrap_or_default(), request());
    }
}
