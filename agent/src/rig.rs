// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! The common rig model (Principle IX).
//!
//! Every radio, whatever its make and however it is reached, is presented
//! through these types, so nothing above the Agent needs to know what radio it
//! is. A radio's own identity (its maker and model) is kept apart from the
//! path used to reach it, so a QSO can name the actual equipment.
//!
//! A [`Radio`] reports what the radio says its live state is (Principle X).
//! A radio whose driver can also tune it is [`Tunable`], and a [`Tuning`]
//! is the whole list of what it may be asked to change. There is no way to
//! ask for a transmission anywhere in the rig model: keying a transmitter
//! comes later, only for the program the person at the computer chooses to
//! control the radio.

use std::fmt;

use serde::Serialize;

/// The highest frequency the Agent accepts from a radio: 300 GHz.
pub const MAX_FREQUENCY_HZ: u64 = 300_000_000_000;

/// An operating mode, as the radio reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Mode {
    /// Upper sideband voice.
    #[serde(rename = "USB")]
    Usb,
    /// Lower sideband voice.
    #[serde(rename = "LSB")]
    Lsb,
    /// CW on the usual sideband.
    #[serde(rename = "CW")]
    Cw,
    /// CW on the opposite sideband.
    #[serde(rename = "CW-R")]
    CwReverse,
    /// The radio's own RTTY mode.
    #[serde(rename = "RTTY")]
    Rtty,
    /// The radio's own RTTY mode, reversed.
    #[serde(rename = "RTTY-R")]
    RttyReverse,
    /// Amplitude modulation.
    #[serde(rename = "AM")]
    Am,
    /// Frequency modulation.
    #[serde(rename = "FM")]
    Fm,
    /// Upper sideband for data (FT8, PSK and the like).
    #[serde(rename = "DATA-USB")]
    DataUsb,
    /// Lower sideband for data.
    #[serde(rename = "DATA-LSB")]
    DataLsb,
    /// FM for data (packet, for example).
    #[serde(rename = "DATA-FM")]
    DataFm,
    /// A mode the rig model has no name for yet.
    #[serde(rename = "OTHER")]
    Other,
}

/// How the Agent reaches a radio. The radio's identity is kept separately.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ControlPath {
    /// Through Hamlib's `rigctld`, run by the Agent for this radio.
    Hamlib {
        /// Hamlib's number for the radio model.
        model: u32,
    },
    /// Over the network, through a FlexRadio's own API.
    Flex,
    /// Over a serial port, in Icom's CI-V protocol, by the Agent's own driver.
    Civ,
    /// The Agent's built-in pretend radio, for trying LabLog without one.
    Pretend,
}

/// What a radio is, as the radio or its profile names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RadioIdentity {
    /// The maker, such as "Icom".
    pub maker: String,
    /// The model, such as "IC-7300".
    pub model: String,
    /// How the Agent reaches it.
    pub path: ControlPath,
}

/// The radio's live state, as it last reported it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Reading {
    /// The receive frequency of the active VFO, in hertz.
    pub frequency_hz: u64,
    /// The mode, if the radio reported one.
    pub mode: Option<Mode>,
    /// The filter width in hertz, if the radio reported one.
    pub passband_hz: Option<u32>,
    /// Whether the radio says it is transmitting; `None` if it can't say.
    pub ptt: Option<bool>,
    /// The active VFO's name, such as "VFOA", if the radio reported one.
    pub vfo: Option<String>,
}

/// Why a radio could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RadioError(pub String);

impl fmt::Display for RadioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RadioError {}

/// A radio the Agent owns. Every radio can be read.
pub trait Radio {
    /// What the radio is and how it is reached.
    fn identity(&self) -> &RadioIdentity;

    /// Ask the radio for its live state.
    ///
    /// # Errors
    ///
    /// The radio did not answer, or answered with something that is not a
    /// valid state (Principle XII).
    fn read(&mut self) -> Result<Reading, RadioError>;

    /// The radio's settings, one by one, if its driver can read them by a
    /// rig definition.
    fn exposes(&mut self) -> Option<&mut dyn Exposes> {
        None
    }
}

/// A radio whose settings the Agent can read one by one, as its rig
/// definition lists them (the settings record, Principle X).
pub trait Exposes {
    /// The radio's rig definition.
    fn definition(&self) -> &crate::rigdef::Definition;

    /// Reads one setting. Returns its bytes as the radio sent them.
    ///
    /// # Errors
    ///
    /// The radio refused, didn't answer, or answered about something else.
    fn read_exposed(
        &mut self,
        setting: &crate::rigdef::SettingSummary,
    ) -> Result<Vec<u8>, RadioError>;

    /// Puts `bytes` in one setting, exactly as the radio sent them when it
    /// was read. Only for a restore the operator asked for and confirmed
    /// (Principle X), and only for a setting the definition says may be
    /// changed and is verified on a real radio.
    ///
    /// # Errors
    ///
    /// The setting may not be written, or the radio refused or didn't
    /// answer.
    fn write_exposed(
        &mut self,
        setting: &crate::rigdef::SettingSummary,
        bytes: &[u8],
    ) -> Result<(), RadioError>;
}

/// A VFO, by the name Hamlib and most radios use for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vfo {
    /// VFO A, or Main.
    A,
    /// VFO B, or Sub.
    B,
}

/// Everything a radio may be asked to change. There is deliberately no
/// transmit request here: tuning never keys a transmitter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tuning {
    /// Tune the selected VFO, in hertz.
    Frequency(u64),
    /// Tune the VFO that isn't selected, in hertz (the transmit frequency in
    /// split).
    OtherFrequency(u64),
    /// Change the selected VFO's mode.
    Mode(Mode),
    /// Select a VFO.
    Select(Vfo),
    /// Turn split on or off.
    Split(bool),
}

/// A radio whose driver can tune it.
pub trait Tunable: Radio {
    /// Ask the radio to make a change. The radio's own state afterwards is
    /// the truth (Principle X), so the caller reads it again rather than
    /// assuming the change took.
    ///
    /// # Errors
    ///
    /// The radio refused, didn't answer, or the change can't be made on this
    /// radio.
    fn tune(&mut self, tuning: Tuning) -> Result<(), RadioError>;
}

/// A radio whose driver can key its transmitter. Only the rigctld server's
/// transmit path uses it, and only when the operator started it with
/// `serve --allow-transmit` (Principle IX).
pub trait Keyable: Tunable {
    /// Key the transmitter (`true`) or unkey it (`false`). Whether the radio
    /// is transmitting afterwards is the radio's to say (Principle X).
    ///
    /// # Errors
    ///
    /// The radio refused or didn't answer: it may or may not be transmitting.
    fn transmit(&mut self, on: bool) -> Result<(), RadioError>;
}
