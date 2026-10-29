// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! A radio on a serial port, driven by one of the Agent's own drivers: Icom
//! CI-V (`civ_radio`) or Yaesu CAT (`cat_radio`). Finding it, the station,
//! the settings record and `check-writes` work the same for either.

use crate::cat_radio::CatRadio;
use crate::civ_radio::{CivRadio, Port, WriteBack};
use crate::civ_serial::SerialPort;
use crate::rig::{Exposes, Keyable, RadioError};
use crate::rigdef::{self, Definition, SettingSummary};

/// What the Agent needs from a radio its own driver found on a serial port.
pub trait SerialRadio: Keyable + Exposes + Send {
    /// The speed the radio answered at.
    fn baud(&self) -> u32;

    /// Reads one setting, writes exactly what it just read back to it, and
    /// reads it again (`check-writes`).
    ///
    /// # Errors
    ///
    /// The setting is one the Agent never changes, or couldn't be read
    /// before the write.
    fn write_back(&mut self, setting: &SettingSummary) -> Result<WriteBack, RadioError>;
}

impl<P: Port + Send> SerialRadio for CivRadio<P> {
    fn baud(&self) -> u32 {
        CivRadio::baud(self)
    }

    fn write_back(&mut self, setting: &SettingSummary) -> Result<WriteBack, RadioError> {
        CivRadio::write_back(self, setting)
    }
}

impl<P: Port + Send> SerialRadio for CatRadio<P> {
    fn baud(&self) -> u32 {
        CatRadio::baud(self)
    }

    fn write_back(&mut self, setting: &SettingSummary) -> Result<WriteBack, RadioError> {
        CatRadio::write_back(self, setting)
    }
}

/// Which protocol families to try on a port, in order: CI-V, unless the
/// port's USB IDs say it is a Yaesu radio's, then CAT. A port the computer
/// says nothing about gets CI-V only, whose RTS stays off: CAT holds RTS
/// on, which an Icom radio may be set to transmit from.
fn families(usb: Option<(u16, u16)>, definitions: &[Definition]) -> Vec<&'static str> {
    let yaesu = usb.is_some_and(|ids| {
        definitions
            .iter()
            .any(|d| d.family == rigdef::YAESU_CAT && d.usb_serial == Some(ids))
    });
    if yaesu {
        vec![rigdef::YAESU_CAT]
    } else {
        vec![rigdef::ICOM_CIV]
    }
}

/// Finds which known radio is on the serial port `name`, with whichever of
/// the Agent's drivers its USB IDs call for (`usb`, if the computer listed
/// them).
///
/// # Errors
///
/// The port couldn't be opened, or no known radio answered on it.
pub fn find(
    name: &str,
    usb: Option<(u16, u16)>,
    definitions: &[Definition],
) -> Result<Box<dyn SerialRadio>, String> {
    let mut why = Vec::new();
    for family in families(usb, definitions) {
        let found: Result<Box<dyn SerialRadio>, String> = if family == rigdef::YAESU_CAT {
            let Some(first) = definitions.iter().find(|d| d.family == family) else {
                continue;
            };
            SerialPort::open_for(name, first).and_then(|port| {
                CatRadio::find(port, definitions)
                    .map(|radio| Box::new(radio) as Box<dyn SerialRadio>)
                    .map_err(|error| error.to_string())
            })
        } else {
            SerialPort::open(name).and_then(|port| {
                CivRadio::find(port, definitions)
                    .map(|radio| Box::new(radio) as Box<dyn SerialRadio>)
                    .map_err(|error| error.to_string())
            })
        };
        match found {
            Ok(radio) => return Ok(radio),
            Err(error) => why.push(error),
        }
    }
    Err(why.join("; "))
}

/// The USB IDs the computer lists for the serial port `name`, if any.
#[must_use]
pub fn usb_ids(name: &str) -> Option<(u16, u16)> {
    serialport::available_ports()
        .ok()?
        .into_iter()
        .find(|port| port.port_name.eq_ignore_ascii_case(name))
        .and_then(|port| match port.port_type {
            serialport::SerialPortType::UsbPort(usb) => Some((usb.vid, usb.pid)),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::families;
    use crate::rigdef;

    #[test]
    fn asks_in_cat_only_on_a_yaesu_radios_port() {
        let definitions = rigdef::built_in().unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(
            families(Some((0x10C4, 0xEA70)), &definitions),
            [rigdef::YAESU_CAT]
        );
        assert_eq!(
            families(Some((0x10C4, 0xEA60)), &definitions),
            [rigdef::ICOM_CIV]
        );
        assert_eq!(
            families(None, &definitions),
            [rigdef::ICOM_CIV],
            "RTS stays off on a port nobody can name"
        );
    }
}
