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

#[cfg(test)]
mod tests {
    use super::SerialPort;

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
