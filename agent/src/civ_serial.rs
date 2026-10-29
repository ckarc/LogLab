// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! A real serial port for the CI-V driver: the COM port a USB radio such as
//! the IC-7300 presents (Silicon Labs `CP210x` on Windows, `/dev/ttyUSB0` on
//! Linux).
//!
//! The port's DTR and RTS lines are held off from the moment it opens. An
//! IC-7300 can be set to key its transmitter or its CW keyer from either line
//! (USB SEND and USB Keying), so a line left on could key the radio. Nothing
//! here ever turns one on (Principle IX).

use std::io::{ErrorKind, Read, Write};
use std::time::{Duration, Instant};

use crate::civ_radio::Port;
use crate::rigdef::Definition;

/// The speed the port opens at, before the driver's speed search sets its own.
const OPENING_BAUD: u32 = 19_200;

/// A serial port the CI-V driver talks through.
pub struct SerialPort {
    name: String,
    port: Box<dyn serialport::SerialPort>,
}

impl SerialPort {
    /// Opens the port named `name`, such as `COM5` or `/dev/ttyUSB0`, with
    /// DTR and RTS off.
    ///
    /// # Errors
    ///
    /// The port doesn't exist, another program has it open, or its lines
    /// couldn't be set.
    pub fn open(name: &str) -> Result<Self, String> {
        let mut port = serialport::new(name, OPENING_BAUD)
            .data_bits(serialport::DataBits::Eight)
            .parity(serialport::Parity::None)
            .stop_bits(serialport::StopBits::One)
            .flow_control(serialport::FlowControl::None)
            .dtr_on_open(false)
            .timeout(Duration::from_millis(20))
            .open()
            .map_err(|error| Self::explain(name, &error))?;
        Self::lines_off(name, port.as_mut())?;
        Ok(Self {
            name: name.to_owned(),
            port,
        })
    }

    fn lines_off(name: &str, port: &mut dyn serialport::SerialPort) -> Result<(), String> {
        port.write_data_terminal_ready(false)
            .and_then(|()| port.write_request_to_send(false))
            .map_err(|error| format!("{name}: can't turn DTR and RTS off: {error}"))
    }

    fn explain(name: &str, error: &serialport::Error) -> String {
        let hint = match error.kind() {
            serialport::ErrorKind::NoDevice => {
                " (is the radio on and its USB cable in? `mode` in a Windows terminal lists COM ports)"
            }
            serialport::ErrorKind::Io(ErrorKind::PermissionDenied) => {
                " (another program has it open: close it, or point that program at `loglab-agent serve`)"
            }
            _ => "",
        };
        format!("can't open {name}: {error}{hint}")
    }
}

impl Port for SerialPort {
    fn set_baud(&mut self, baud: u32) -> Result<(), String> {
        self.port
            .set_baud_rate(baud)
            .map_err(|error| format!("{}: can't set {baud} baud: {error}", self.name))?;
        // Some drivers reset the lines when the speed changes.
        Self::lines_off(&self.name, self.port.as_mut())?;
        // Whatever arrived at the old speed is noise now.
        self.port
            .clear(serialport::ClearBuffer::Input)
            .map_err(|error| format!("{}: {error}", self.name))
    }

    fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.port
            .write_all(bytes)
            .and_then(|()| self.port.flush())
            .map_err(|error| format!("{}: can't send: {error}", self.name))
    }

    fn receive(&mut self, wait: Duration) -> Result<Vec<u8>, String> {
        let deadline = Instant::now() + wait;
        let mut got = Vec::new();
        let mut buffer = [0_u8; 256];
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(got);
            }
            self.port
                .set_timeout(left)
                .map_err(|error| format!("{}: {error}", self.name))?;
            match self.port.read(&mut buffer) {
                // Bytes came: hand them over now, the driver asks again.
                Ok(count) if count > 0 => {
                    got.extend_from_slice(&buffer[..count]);
                    return Ok(got);
                }
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::TimedOut => return Ok(got),
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(format!("{}: can't read: {error}", self.name)),
            }
        }
    }
}

/// A serial port that may be a known radio's: its USB IDs match a radio
/// definition's serial bridge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// The port's name, such as `COM5`.
    pub port: String,
    /// The USB serial number, such as `IC-7300_02065962`, if it has one.
    pub serial: Option<String>,
}

/// The serial ports on this computer that may be a known radio's, the likeliest
/// first.
///
/// # Errors
///
/// The computer's serial ports couldn't be listed.
pub fn radio_ports(definitions: &[Definition]) -> Result<Vec<Candidate>, String> {
    let ports = serialport::available_ports()
        .map_err(|error| format!("can't list this computer's serial ports: {error}"))?;
    Ok(candidates(&ports, definitions))
}

/// The ports among `ports` whose USB IDs match one of `definitions`. Many
/// devices share a serial bridge chip's IDs, so a port whose USB serial
/// number names the radio's model (an IC-7300's is `IC-7300_` and a number)
/// comes first. Only the radio's own answer to "who are you?" decides.
///
/// A radio with more than one serial port behind its USB (an IC-9700's
/// USB (A) and USB (B)) has only its control port listed: the others may
/// carry data the radio transmits, so the Agent never opens them.
fn candidates(ports: &[serialport::SerialPortInfo], definitions: &[Definition]) -> Vec<Candidate> {
    let mut found: Vec<(bool, Candidate)> = ports
        .iter()
        .filter_map(|port| match &port.port_type {
            serialport::SerialPortType::UsbPort(usb) => {
                let serial = usb.serial_number.as_deref().unwrap_or_default();
                let other_port = definitions.iter().any(|d| {
                    d.usb_serial == Some((usb.vid, usb.pid))
                        && serial.contains(&d.model)
                        && d.usb_serial_number_ends
                            .as_deref()
                            .is_some_and(|end| !ends_with_part(serial, end))
                });
                if other_port {
                    return None;
                }
                let named = definitions
                    .iter()
                    .filter(|d| d.usb_serial == Some((usb.vid, usb.pid)))
                    .map(|d| {
                        usb.serial_number
                            .as_deref()
                            .is_some_and(|serial| serial.contains(&d.model))
                    })
                    .reduce(|a, b| a || b)?;
                Some((
                    named,
                    Candidate {
                        port: port.port_name.clone(),
                        serial: usb.serial_number.clone(),
                    },
                ))
            }
            _ => None,
        })
        .collect();
    // Named ones first; otherwise in the order the computer listed them.
    found.sort_by_key(|(named, _)| !*named);
    found.into_iter().map(|(_, candidate)| candidate).collect()
}

/// Whether a USB serial number's last part, after a space or an underscore
/// (Windows writes `IC-9700 13001202 A` as `IC-9700_13001202_A`), is `end`.
fn ends_with_part(serial: &str, end: &str) -> bool {
    serial
        .trim_end()
        .rsplit([' ', '_'])
        .next()
        .is_some_and(|last| last.eq_ignore_ascii_case(end))
}

#[cfg(test)]
mod tests {
    use super::{Candidate, SerialPort, candidates};
    use crate::rigdef;

    fn usb(name: &str, vid: u16, pid: u16, serial: Option<&str>) -> serialport::SerialPortInfo {
        serialport::SerialPortInfo {
            port_name: name.to_owned(),
            port_type: serialport::SerialPortType::UsbPort(serialport::UsbPortInfo {
                vid,
                pid,
                serial_number: serial.map(str::to_owned),
                manufacturer: None,
                product: None,
            }),
        }
    }

    #[test]
    fn finds_a_radios_port_by_its_usb_ids_named_ones_first() {
        let definitions = rigdef::built_in().unwrap_or_else(|error| unreachable!("{error}"));
        let ports = [
            serialport::SerialPortInfo {
                port_name: "COM3".to_owned(),
                port_type: serialport::SerialPortType::PciPort,
            },
            usb("COM4", 0x10C4, 0xEA60, Some("0001")),
            usb("COM6", 0x0403, 0x6001, Some("FT1234")),
            usb("COM5", 0x10C4, 0xEA60, Some("IC-7300_02065962")),
        ];
        assert_eq!(
            candidates(&ports, &definitions),
            [
                Candidate {
                    port: "COM5".to_owned(),
                    serial: Some("IC-7300_02065962".to_owned())
                },
                Candidate {
                    port: "COM4".to_owned(),
                    serial: Some("0001".to_owned())
                },
            ]
        );
        assert!(candidates(&ports[..1], &definitions).is_empty());
    }

    #[test]
    fn opens_only_an_ic_9700s_usb_a_port() {
        let definitions = rigdef::built_in().unwrap_or_else(|error| unreachable!("{error}"));
        let ports = [
            usb("COM7", 0x10C4, 0xEA60, Some("IC-9700 13001202 B")),
            usb("COM8", 0x10C4, 0xEA60, Some("IC-9700_13001202_B")),
            usb("COM9", 0x10C4, 0xEA60, Some("IC-9700_13001202_A")),
            usb("COM10", 0x10C4, 0xEA60, Some("IC-9700 13001202 A")),
            usb("COM5", 0x10C4, 0xEA60, Some("IC-7300_02065962")),
        ];
        let ports: Vec<String> = candidates(&ports, &definitions)
            .into_iter()
            .map(|candidate| candidate.port)
            .collect();
        assert_eq!(ports, ["COM9", "COM10", "COM5"], "USB (B) is never opened");
    }

    #[test]
    fn a_missing_port_is_refused_by_name() {
        let error = SerialPort::open("LABLOG-NO-SUCH-PORT")
            .err()
            .unwrap_or_else(|| unreachable!("no such port exists"));
        assert!(
            error.starts_with("can't open LABLOG-NO-SUCH-PORT"),
            "{error}"
        );
    }
}
