// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Keeps "LabLog Radio" from being one of Windows' default sound devices:
//! only the radio programs the Agent sets up use it, by name, and Windows'
//! own sounds and every other program stay on the computer's speakers and
//! microphone (brian, 2026-11-28).
//!
//! Windows gives a newly arrived sound device "a first opportunity to be
//! the default", and ranks USB first among buses (Microsoft Learn, "Default
//! Audio Endpoint Selection"), so it made the card its default playback
//! device when the card first appeared at FL-1. The documented way to stop
//! that, `PKEY_AudioDevice_NeverSetAsDefaultEndpoint`, is written by a
//! driver's INF, and the card uses Windows' own USB audio driver.
//!
//! So [`Keeper`] looks at each default (playback and recording, console and
//! communications) every [`LOOK_EVERY`], remembers the last one that isn't
//! the card, and when the card has become one, puts that one back. Its
//! memory is kept in a file (`sound-defaults.txt` in the records folder),
//! so it holds across restarts. With nothing remembered, it says so and
//! changes nothing: it never guesses which device is the computer's own.
//!
//! Reading the defaults uses Windows' documented `IMMDeviceEnumator`;
//! putting one back uses `IPolicyConfig::SetDefaultEndpoint`, which is
//! undocumented (the interface Windows' Sound settings uses; its layout
//! here is from the MIT-licensed `com-policy-config` crate's port of
//! `PolicyConfig.h`). It set FL-1's default back on 2026-11-28.

// Only Windows looks after its defaults; elsewhere this is built for its
// tests.
#![cfg_attr(not(windows), allow(dead_code))]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::sound_card::{NAME as CARD, is_card};

/// How often the defaults are looked at.
pub const LOOK_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

/// Which way sound goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Flow {
    /// Playback.
    Render,
    /// Recording.
    Capture,
}

/// Which default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    /// Windows' own sounds, and most programs.
    Console,
    /// Calls.
    Communications,
}

/// Every default the Keeper looks after.
pub const ALL: [(Flow, Role); 4] = [
    (Flow::Render, Role::Console),
    (Flow::Render, Role::Communications),
    (Flow::Capture, Role::Console),
    (Flow::Capture, Role::Communications),
];

impl Flow {
    fn word(self) -> &'static str {
        match self {
            Self::Render => "playback",
            Self::Capture => "recording",
        }
    }
}

impl Role {
    fn word(self) -> &'static str {
        match self {
            Self::Console => "default",
            Self::Communications => "communications",
        }
    }
}

/// A sound device as Windows names it: its endpoint id and its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// The endpoint id (`IMMDevice::GetId`).
    pub id: String,
    /// Its name, as the Sound settings show it.
    pub name: String,
}

impl Device {
    /// Whether it is the Agent's own card.
    #[must_use]
    pub fn is_card(&self) -> bool {
        is_card(&self.name)
    }
}

/// What to do about one default, just looked at.
#[derive(Debug, PartialEq, Eq)]
pub enum Do {
    /// Nothing.
    Nothing,
    /// Put this device back as the default.
    PutBack(Device),
    /// The card is the default and nothing is known to put back: say so,
    /// once.
    Tell,
}

/// The last default that wasn't the card, for each default.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Keeper {
    kept: BTreeMap<(Flow, Role), Device>,
    told: BTreeMap<(Flow, Role), bool>,
}

impl Keeper {
    /// What to do now the default for `which` is `now` (None: there is
    /// none). Remembers `now` if it isn't the card. Returns whether the
    /// memory changed too, so it can be saved.
    pub fn saw(&mut self, which: (Flow, Role), now: Option<&Device>) -> (Do, bool) {
        match now {
            Some(device) if device.is_card() => match self.kept.get(&which) {
                Some(kept) => (Do::PutBack(kept.clone()), false),
                None if self.told.insert(which, true) != Some(true) => (Do::Tell, false),
                None => (Do::Nothing, false),
            },
            Some(device) => {
                self.told.remove(&which);
                let changed = self.kept.get(&which) != Some(device);
                if changed {
                    self.kept.insert(which, device.clone());
                }
                (Do::Nothing, changed)
            }
            None => (Do::Nothing, false),
        }
    }

    /// The memory as the file keeps it: one line a default, `flow role id`
    /// then a tab and the name.
    #[must_use]
    pub fn text(&self) -> String {
        use std::fmt::Write as _;
        let mut text = String::new();
        for ((flow, role), device) in &self.kept {
            let _ = writeln!(
                text,
                "{} {} {}\t{}",
                flow.word(),
                role.word(),
                device.id,
                device.name
            );
        }
        text
    }

    /// The memory read back from [`Keeper::text`]; lines it can't read are
    /// left out.
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        let mut keeper = Self::default();
        for line in text.lines() {
            let Some((head, name)) = line.split_once('\t') else {
                continue;
            };
            let mut words = head.splitn(3, ' ');
            let (Some(flow), Some(role), Some(id)) = (words.next(), words.next(), words.next())
            else {
                continue;
            };
            let flow = match flow {
                "playback" => Flow::Render,
                "recording" => Flow::Capture,
                _ => continue,
            };
            let role = match role {
                "default" => Role::Console,
                "communications" => Role::Communications,
                _ => continue,
            };
            let device = Device {
                id: id.to_owned(),
                name: name.to_owned(),
            };
            if !device.is_card() && !device.id.is_empty() {
                keeper.kept.insert((flow, role), device);
            }
        }
        keeper
    }
}

/// Where the Keeper's memory is kept, in the records folder `records`.
#[must_use]
pub fn path(records: &Path) -> PathBuf {
    records.join("sound-defaults.txt")
}

/// What to say when the card is a default and nothing is known to put
/// back.
#[must_use]
pub fn tell(which: (Flow, Role)) -> String {
    format!(
        "{CARD}: Windows made it the {} {} device, and the Agent doesn't know which device \
         was the computer's own.\n-> In Windows' Sound settings, pick the computer's own \
         device: the Agent keeps it from then on.",
        which.0.word(),
        which.1.word()
    )
}

/// What to say once a default is put back.
#[must_use]
pub fn put_back(which: (Flow, Role), device: &Device) -> String {
    format!(
        "{CARD}: Windows made it the {} {} device; the Agent put \"{}\" back (only radio \
         programs use {CARD}).",
        which.0.word(),
        which.1.word(),
        device.name
    )
}

/// Keeps the card from being a default, on a thread of its own, for as
/// long as the Agent runs.
#[cfg(windows)]
pub fn keep_off(records: PathBuf) {
    std::thread::spawn(move || windows_api::keep_off(&records));
}

/// The Windows calls: COM, the only unsafe code here. The COM interface
/// macro's own code casts between pointers, which Clippy flags.
#[cfg(windows)]
#[allow(unsafe_code, clippy::transmute_ptr_to_ptr)]
mod windows_api {
    use std::ffi::c_void;
    use std::path::Path;

    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Media::Audio::{
        ERole, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, eCapture, eCommunications,
        eConsole, eRender,
    };
    use windows::Win32::System::Com::StructuredStorage::{
        PropVariantClear, PropVariantToStringAlloc,
    };
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
        STGM_READ,
    };
    use windows::core::{GUID, HRESULT, IUnknown, IUnknown_Vtbl, PCWSTR, interface};

    use super::{Device, Do, Flow, Keeper, LOOK_EVERY, Role, path, put_back, tell};

    /// `PolicyConfigClient`'s class id (`PolicyConfig.h`, as ported by the
    /// MIT-licensed `com-policy-config` crate).
    const POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870a_f99c_171d_4f9e_af0d_e63d_f40c_2bc9);

    /// `IPolicyConfig` (`PolicyConfig.h`), undocumented, its methods in
    /// their order there (named here as Rust names them). Only
    /// `SetDefaultEndpoint` is called; the others are here for their places
    /// in the table, with their arguments as plain pointers. The macro's
    /// own code casts between pointers.
    #[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
    unsafe trait IPolicyConfig: IUnknown {
        fn get_mix_format(&self, device: PCWSTR, format: *mut *mut c_void) -> HRESULT;
        fn get_device_format(
            &self,
            device: PCWSTR,
            default: i32,
            format: *mut *mut c_void,
        ) -> HRESULT;
        fn reset_device_format(&self, device: PCWSTR) -> HRESULT;
        fn set_device_format(
            &self,
            device: PCWSTR,
            endpoint: *mut c_void,
            mix: *mut c_void,
        ) -> HRESULT;
        fn get_processing_period(
            &self,
            device: PCWSTR,
            default: i32,
            default_period: *mut i64,
            min_period: *mut i64,
        ) -> HRESULT;
        fn set_processing_period(&self, device: PCWSTR, period: *mut i64) -> HRESULT;
        fn get_share_mode(&self, device: PCWSTR, mode: *mut c_void) -> HRESULT;
        fn set_share_mode(&self, device: PCWSTR, mode: *mut c_void) -> HRESULT;
        fn get_property_value(
            &self,
            device: PCWSTR,
            key: *const c_void,
            value: *mut c_void,
        ) -> HRESULT;
        fn set_property_value(
            &self,
            device: PCWSTR,
            key: *const c_void,
            value: *mut c_void,
        ) -> HRESULT;
        fn set_default_endpoint(&self, device: PCWSTR, role: ERole) -> HRESULT;
        fn set_endpoint_visibility(&self, device: PCWSTR, visible: i32) -> HRESULT;
    }

    /// Looks after the defaults until the Agent ends.
    pub fn keep_off(records: &Path) {
        // SAFETY: first COM call on this thread, which is the Keeper's own
        // and stays in the multithreaded apartment for its whole life
        // (Microsoft Learn: CoInitializeEx).
        let joined = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if joined.is_err() {
            println!("Sound defaults: not looked after (COM: {joined:?}).");
            return;
        }
        // SAFETY: COM is initialised on this thread (above).
        let made: windows::core::Result<IMMDeviceEnumerator> =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) };
        let devices = match made {
            Ok(devices) => devices,
            Err(error) => {
                println!("Sound defaults: not looked after ({error}).");
                return;
            }
        };
        let file = path(records);
        let mut keeper = Keeper::from_text(&std::fs::read_to_string(&file).unwrap_or_default());
        loop {
            let mut changed = false;
            for which in super::ALL {
                let (todo, remembered) = keeper.saw(which, default(&devices, which).as_ref());
                changed |= remembered;
                match todo {
                    Do::Nothing => {}
                    Do::Tell => crate::print_event(&tell(which)),
                    Do::PutBack(device) => match set_default(&device.id, which.1) {
                        Ok(()) => crate::print_event(&put_back(which, &device)),
                        Err(why) => crate::print_event(&format!(
                            "Sound defaults: couldn't put \"{}\" back ({why}).",
                            device.name
                        )),
                    },
                }
            }
            if changed {
                let _ = std::fs::create_dir_all(records);
                if let Err(error) = std::fs::write(&file, keeper.text()) {
                    println!("Sound defaults: {}: {error}", file.display());
                }
            }
            std::thread::sleep(LOOK_EVERY);
        }
    }

    fn role(role: Role) -> ERole {
        match role {
            Role::Console => eConsole,
            Role::Communications => eCommunications,
        }
    }

    /// The default device for `which`, if there is one.
    fn default(devices: &IMMDeviceEnumerator, which: (Flow, Role)) -> Option<Device> {
        let flow = match which.0 {
            Flow::Render => eRender,
            Flow::Capture => eCapture,
        };
        // SAFETY: a documented call on a live enumerator (Microsoft Learn:
        // IMMDeviceEnumerator::GetDefaultAudioEndpoint); with no such
        // device it returns an error, taken as none.
        let device = unsafe { devices.GetDefaultAudioEndpoint(flow, role(which.1)) }.ok()?;
        Some(Device {
            id: id(&device)?,
            name: name(&device).unwrap_or_default(),
        })
    }

    /// The device's endpoint id.
    fn id(device: &IMMDevice) -> Option<String> {
        // SAFETY: GetId gives a string COM allocated, read once here and
        // then freed with CoTaskMemFree, as Microsoft Learn says to.
        unsafe {
            let id = device.GetId().ok()?;
            let text = id.to_string().ok();
            CoTaskMemFree(Some(id.0 as *const c_void));
            text
        }
    }

    /// The device's name, as the Sound settings show it.
    fn name(device: &IMMDevice) -> Option<String> {
        // SAFETY: the property store is opened to read; the value read is
        // cleared after use, and its string, allocated by
        // PropVariantToStringAlloc, freed with CoTaskMemFree (Microsoft
        // Learn: IMMDevice::OpenPropertyStore, PropVariantToStringAlloc).
        unsafe {
            let store = device.OpenPropertyStore(STGM_READ).ok()?;
            let mut value = store.GetValue(&PKEY_Device_FriendlyName).ok()?;
            let text = PropVariantToStringAlloc(&raw const value).ok().map(|text| {
                let name = text.to_string().unwrap_or_default();
                CoTaskMemFree(Some(text.0 as *const c_void));
                name
            });
            let _ = PropVariantClear(&raw mut value);
            text
        }
    }

    /// Makes the device `id` Windows' default for `which` role.
    fn set_default(id: &str, which: Role) -> Result<(), String> {
        let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: COM is initialised on this thread; `wide` is a
        // nul-terminated UTF-16 string that outlives the call. The
        // interface's layout is PolicyConfig.h's (see `IPolicyConfig`).
        unsafe {
            let policy: IPolicyConfig = CoCreateInstance(&POLICY_CONFIG_CLIENT, None, CLSCTX_ALL)
                .map_err(|error| error.to_string())?;
            policy
                .set_default_endpoint(PCWSTR(wide.as_ptr()), role(which))
                .ok()
                .map_err(|error| error.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Device, Do, Flow, Keeper, Role};

    fn device(id: &str, name: &str) -> Device {
        Device {
            id: id.to_owned(),
            name: name.to_owned(),
        }
    }

    const PLAYBACK: (Flow, Role) = (Flow::Render, Role::Console);

    #[test]
    fn the_card_as_a_default_is_put_back_to_the_last_other_device() {
        let mut keeper = Keeper::default();
        let speakers = device(
            "{0.0.0}.{a}",
            "Speakers (Thinkcentre TIO24Gen3 for USB-audio)",
        );
        let card = device("{0.0.0}.{b}", "LabLog TX (LabLog Radio)");
        assert_eq!(keeper.saw(PLAYBACK, Some(&speakers)), (Do::Nothing, true));
        assert_eq!(keeper.saw(PLAYBACK, Some(&speakers)), (Do::Nothing, false));
        assert_eq!(
            keeper.saw(PLAYBACK, Some(&card)),
            (Do::PutBack(speakers.clone()), false)
        );
        // Again, if Windows takes it again.
        assert_eq!(
            keeper.saw(PLAYBACK, Some(&card)),
            (Do::PutBack(speakers), false)
        );
        // Each default is looked after on its own.
        assert_eq!(
            keeper.saw((Flow::Capture, Role::Console), Some(&card)),
            (Do::Tell, false)
        );
    }

    #[test]
    fn with_nothing_known_it_says_so_once_and_changes_nothing() {
        let mut keeper = Keeper::default();
        let card = device("{0.0.0}.{b}", "LabLog TX (LabLog Radio)");
        assert_eq!(keeper.saw(PLAYBACK, Some(&card)), (Do::Tell, false));
        assert_eq!(keeper.saw(PLAYBACK, Some(&card)), (Do::Nothing, false));
        assert_eq!(keeper.saw(PLAYBACK, None), (Do::Nothing, false));
        // Once the operator picks another, that one is kept.
        let other = device("{0.0.0}.{c}", "Headphones");
        assert_eq!(keeper.saw(PLAYBACK, Some(&other)), (Do::Nothing, true));
        assert_eq!(
            keeper.saw(PLAYBACK, Some(&card)),
            (Do::PutBack(other), false)
        );
    }

    #[test]
    fn its_memory_is_kept_in_a_file_and_read_back() {
        let mut keeper = Keeper::default();
        let speakers = device("{0.0.0}.{a}", "Speakers (Thinkcentre)");
        let microphone = device("{0.0.1}.{d}", "Microphone (Thinkcentre)");
        keeper.saw(PLAYBACK, Some(&speakers));
        keeper.saw((Flow::Capture, Role::Communications), Some(&microphone));
        let text = keeper.text();
        assert_eq!(
            text,
            "playback default {0.0.0}.{a}\tSpeakers (Thinkcentre)\n\
             recording communications {0.0.1}.{d}\tMicrophone (Thinkcentre)\n"
        );
        assert_eq!(Keeper::from_text(&text), keeper);
        // The card is never kept, nor a line it can't read.
        let read = Keeper::from_text(
            "playback default {x}\tSpeakers (LabLog Radio)\nnonsense\nplayback sideways {y}\tZ\n",
        );
        assert_eq!(read, Keeper::default());
    }
}
