// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Which FlexRadio, if any, this computer's station uses, as the operator
//! picks it: with the tray's "Use FlexRadio" checkmark (`flex on` and
//! `flex off` at the station's console), or by typing `flex`, `flex use
//! SERIAL` and `flex stop`.
//!
//! With the checkmark on, the station does the footwork once a radio
//! program (WSJT-X, `JS8Call`, fldigi, VARA, MMSSTV, Winlink Express) is
//! open: it takes the one free FlexRadio on the network (asking which, when
//! there are several), opens SmartSDR if it isn't open, and uses its slice.
//!
//! USB radios are found as they always were. A FlexRadio on the network may
//! be another station's, so the Agent uses one only once the operator picks
//! it here, or starts the Agent with `--flex` (Principle IX). A FlexRadio
//! another computer uses is not offered and can't be picked.
//!
//! A pick from here is not kept: the next start of the Agent (at the next
//! Windows sign-in, say) begins with the checkmark off and USB radios, so a
//! computer never picks a FlexRadio by itself. A FlexRadio picked with
//! `flex use` that is lost (SmartSDR closed, so its slice went, or the
//! radio turned off) goes back to USB radios; with the checkmark on, the
//! station waits for a FlexRadio again instead, until it is turned off.

use std::net::IpAddr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::flex::Discovered;

/// What the operator picked, shared between the console that hears it and
/// the station that acts on it.
#[derive(Debug, Default)]
pub struct Pick {
    /// The FlexRadio to use (its serial number, an address, `any`, or
    /// `pretend`), or `None` for USB radios.
    chosen: Mutex<Option<String>>,
    /// Whether it came from `--flex`, which stays for the whole run.
    given: AtomicBool,
    /// Whether the pick changed since the station last looked.
    changed: AtomicBool,
    /// The radios heard by the last `flex`, so `flex use` is checked
    /// against what was offered.
    heard: Mutex<Vec<Discovered>>,
    /// The "Use FlexRadio" checkmark (`flex on`).
    checked: AtomicBool,
    /// Whether the Agent opened SmartSDR for the radio programs open now,
    /// so it opens it once for them, not again after someone closes it.
    opened: AtomicBool,
    /// The free radios last asked about, so the operator is asked which
    /// one once, not on every look.
    asked: Mutex<Vec<String>>,
}

impl Pick {
    /// A pick that starts with `--flex`'s radio, if it was given.
    #[must_use]
    pub fn starting_with(flex: Option<&str>) -> Self {
        let pick = Self::default();
        if let Some(flex) = flex
            && let Ok(mut chosen) = pick.chosen.lock()
        {
            *chosen = Some(flex.to_owned());
            pick.given.store(true, Ordering::SeqCst);
        }
        pick
    }

    /// The FlexRadio to use now, if any.
    #[must_use]
    pub fn current(&self) -> Option<String> {
        self.chosen.lock().ok().and_then(|chosen| chosen.clone())
    }

    /// Whether the pick changed since the last `take_changed`, clearing it.
    pub fn take_changed(&self) -> bool {
        self.changed.swap(false, Ordering::SeqCst)
    }

    /// Whether the pick changed, without clearing it.
    #[must_use]
    pub fn has_changed(&self) -> bool {
        self.changed.load(Ordering::SeqCst)
    }

    /// Whether the "Use FlexRadio" checkmark is on.
    #[must_use]
    pub fn checked(&self) -> bool {
        self.checked.load(Ordering::SeqCst)
    }

    /// `flex on`: the "Use FlexRadio" checkmark. Says what becomes of it.
    pub fn on(&self) -> String {
        if !self.checked.swap(true, Ordering::SeqCst) {
            self.opened.store(false, Ordering::SeqCst);
            if let Ok(mut asked) = self.asked.lock() {
                asked.clear();
            }
            self.given.store(false, Ordering::SeqCst);
            self.changed.store(true, Ordering::SeqCst);
        }
        "FlexRadio: Use FlexRadio is on. When a radio program opens (WSJT-X, JS8Call, \
         fldigi, VARA, MMSSTV, Winlink Express), the Agent takes the free FlexRadio, opens \
         SmartSDR if it isn't open, and uses its slice. flex off goes back to USB radios."
            .to_owned()
    }

    /// Takes `serial` as the FlexRadio to use, found by the footwork the
    /// checkmark asks for: the station is already looking for it.
    pub fn chose(&self, serial: &str) {
        if let Ok(mut slot) = self.chosen.lock() {
            *slot = Some(serial.to_owned());
        }
    }

    /// Whether to open SmartSDR for the radio programs open now: `true`
    /// the first time it is asked while they are open. `programs_open`
    /// false starts over, for the next radio program to open.
    pub fn open_smartsdr_now(&self, programs_open: bool) -> bool {
        if !programs_open {
            self.opened.store(false, Ordering::SeqCst);
            return false;
        }
        !self.opened.swap(true, Ordering::SeqCst)
    }

    /// Whether to ask the operator which of the `free` radios to use:
    /// only when they differ from the ones last asked about.
    pub fn ask_which(&self, free: &[String]) -> bool {
        let Ok(mut asked) = self.asked.lock() else {
            return false;
        };
        if asked.as_slice() == free {
            return false;
        }
        *asked = free.to_vec();
        true
    }

    fn set(&self, chosen: Option<String>) {
        if let Ok(mut slot) = self.chosen.lock() {
            *slot = chosen;
        }
        self.given.store(false, Ordering::SeqCst);
        self.changed.store(true, Ordering::SeqCst);
    }

    /// The FlexRadio picked from here was lost: back to USB radios, and
    /// what to say about it. `None` for one given with `--flex`, which the
    /// station keeps looking for, as it does for a USB radio.
    pub fn lost(&self) -> Option<String> {
        if self.given.load(Ordering::SeqCst) {
            return None;
        }
        let was = self.current()?;
        if let Ok(mut slot) = self.chosen.lock() {
            *slot = None;
        }
        if self.checked() {
            return Some(format!(
                "FlexRadio: {was} is gone (SmartSDR closed, or the radio is off). Use \
                 FlexRadio is still on, so the Agent waits for a FlexRadio again (flex off \
                 goes back to USB radios)."
            ));
        }
        Some(format!(
            "FlexRadio: {was} is gone (SmartSDR closed, or the radio is off), so the Agent \
             goes back to USB radios."
        ))
    }

    /// Keeps what `flex` heard, for `flex use` to check against.
    pub fn heard(&self, radios: &[Discovered]) {
        if let Ok(mut heard) = self.heard.lock() {
            *heard = radios.to_vec();
        }
    }

    /// `flex use SERIAL`: picks the FlexRadio of that serial number, if the
    /// last `flex` heard it and no other computer uses it. Says what
    /// became of it.
    pub fn use_serial(&self, serial: &str, here: impl Fn(IpAddr) -> Option<IpAddr>) -> String {
        let heard = self.heard.lock().map(|h| h.clone()).unwrap_or_default();
        let Some(radio) = heard.iter().find(|radio| radio.serial == serial) else {
            return format!(
                "FlexRadio: {serial} wasn't heard on the network; type flex to look again."
            );
        };
        if radio.used_elsewhere(here(radio.address.ip())) == Some(true) {
            return format!(
                "FlexRadio: {} is in use by another computer, so it was left alone.",
                described(radio)
            );
        }
        self.set(Some(radio.serial.clone()));
        format!(
            "FlexRadio: using {}. Open SmartSDR on this computer with a slice; the Agent \
             starts once it has one, and goes back to USB radios when SmartSDR closes \
             (flex stop does it now).",
            described(radio)
        )
    }

    /// `flex stop` (or `flex off`, the checkmark turned off): back to USB
    /// radios. Says what became of it.
    pub fn stop(&self) -> String {
        let was_checked = self.checked.swap(false, Ordering::SeqCst);
        if self.current().is_none() && !was_checked {
            return "FlexRadio: none in use; the Agent uses USB radios.".to_owned();
        }
        self.set(None);
        "FlexRadio: stopped using it; the Agent goes back to USB radios (once nothing \
         is transmitting)."
            .to_owned()
    }
}

/// A FlexRadio as the operator knows it: model, name, serial and address.
#[must_use]
pub fn described(radio: &Discovered) -> String {
    let name = radio
        .name
        .as_deref()
        .map(|name| format!(" \"{name}\""))
        .unwrap_or_default();
    format!(
        "{}{name} (serial {}) at {}",
        radio.model,
        radio.serial,
        radio.address.ip()
    )
}

/// How the station asks which FlexRadio to use, when the checkmark is on
/// and several are free; the tray reads it and offers each.
pub const WHICH: &str = "FlexRadio: more than one is free; which one should the Agent use?";

/// The start of each line of `flex`'s answer that offers a radio; the
/// tray reads these.
pub const OFFER: &str = "  free: ";
/// The end of an offered line: what to type to use it.
pub const USE: &str = " -> flex use ";

/// What `flex` says about the radios it heard: each one free to use, with
/// how to use it, and each in use by another computer. `here` is this
/// computer's address toward a radio.
#[must_use]
pub fn offered(radios: &[Discovered], here: impl Fn(IpAddr) -> Option<IpAddr>) -> String {
    if radios.is_empty() {
        return "FlexRadio: none said it is on this network.\n\
                -> Is it on, and on this computer's network? Windows may ask once to let the \
                Agent use the network: allow it on private networks."
            .to_owned();
    }
    let mut lines = vec![format!("FlexRadio: heard {} on the network.", radios.len())];
    for radio in radios {
        let line = match radio.used_elsewhere(here(radio.address.ip())) {
            Some(true) => format!("  in use by another computer: {}", described(radio)),
            Some(false) => format!("{OFFER}{}{USE}{}", described(radio), radio.serial),
            None => format!(
                "{OFFER}{} (it doesn't say whether another computer uses it){USE}{}",
                described(radio),
                radio.serial
            ),
        };
        lines.push(line);
    }
    lines.join("\n")
}

/// The `radios` no other computer uses, the ones the checkmark may take.
#[must_use]
pub fn free(radios: &[Discovered], here: impl Fn(IpAddr) -> Option<IpAddr>) -> Vec<&Discovered> {
    radios
        .iter()
        .filter(|radio| radio.used_elsewhere(here(radio.address.ip())) != Some(true))
        .collect()
}

/// The radios `flex`'s answer offers, as (serial number, what it is), for
/// the tray.
#[must_use]
#[cfg_attr(not(windows), allow(dead_code))]
pub fn offers(said: &str) -> Vec<(String, String)> {
    said.lines()
        .filter_map(|line| {
            let rest = line.trim_end().strip_prefix(OFFER)?;
            let (what, serial) = rest.rsplit_once(USE)?;
            let serial = serial.trim();
            (!serial.is_empty() && !serial.contains(char::is_whitespace))
                .then(|| (serial.to_owned(), what.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn radio(serial: &str, used_from: Option<Vec<&str>>) -> Discovered {
        Discovered {
            model: "FLEX-6600".to_owned(),
            serial: serial.to_owned(),
            name: Some("Field Lab".to_owned()),
            address: "192.168.1.50:4992"
                .parse()
                .unwrap_or_else(|_| unreachable!()),
            used_from: used_from.map(|ips| {
                ips.iter()
                    .filter_map(|ip| ip.parse().ok())
                    .collect::<Vec<IpAddr>>()
            }),
        }
    }

    fn here(_: IpAddr) -> Option<IpAddr> {
        "192.168.1.54".parse().ok()
    }

    #[test]
    fn offers_only_radios_no_other_computer_uses() {
        let radios = [
            radio("FREE-1", Some(vec![])),
            radio("MINE-2", Some(vec!["192.168.1.54"])),
            radio("BUSY-3", Some(vec!["192.168.1.60"])),
            radio("QUIET-4", None),
        ];
        let said = offered(&radios, here);
        assert!(said.starts_with("FlexRadio: heard 4 on the network."));
        assert!(
            said.contains("in use by another computer: FLEX-6600 \"Field Lab\" (serial BUSY-3)")
        );
        let serials: Vec<String> = offers(&said)
            .into_iter()
            .map(|(serial, _)| serial)
            .collect();
        assert_eq!(serials, ["FREE-1", "MINE-2", "QUIET-4"]);
        let free: Vec<&str> = free(&radios, here)
            .iter()
            .map(|radio| radio.serial.as_str())
            .collect();
        assert_eq!(free, ["FREE-1", "MINE-2", "QUIET-4"]);
        assert!(offered(&[], here).starts_with("FlexRadio: none said"));
        assert!(offers("free: nothing -> flex use two words").is_empty());
    }

    #[test]
    fn picks_a_heard_free_radio_and_goes_back_to_usb() {
        let pick = Pick::default();
        assert_eq!(pick.current(), None);
        assert!(pick.use_serial("FREE-1", here).contains("wasn't heard"));
        pick.heard(&[
            radio("FREE-1", Some(vec![])),
            radio("BUSY-3", Some(vec!["192.168.1.60"])),
        ]);
        assert!(
            pick.use_serial("BUSY-3", here)
                .contains("in use by another computer")
        );
        assert!(!pick.take_changed());
        assert!(
            pick.use_serial("FREE-1", here)
                .starts_with("FlexRadio: using FLEX-6600")
        );
        assert_eq!(pick.current().as_deref(), Some("FREE-1"));
        assert!(pick.has_changed());
        assert!(pick.take_changed());
        assert!(!pick.take_changed());
        // Lost: back to USB radios, said once.
        assert!(
            pick.lost()
                .is_some_and(|said| said.contains("goes back to USB radios"))
        );
        assert_eq!(pick.current(), None);
        assert_eq!(pick.lost(), None);
        // Stopped by the operator.
        assert!(
            pick.use_serial("FREE-1", here)
                .starts_with("FlexRadio: using")
        );
        assert!(pick.take_changed());
        assert!(pick.stop().contains("goes back to USB radios"));
        assert!(pick.take_changed());
        assert!(pick.stop().contains("none in use"));
    }

    #[test]
    fn the_checkmark_waits_for_a_flexradio_until_it_is_turned_off() {
        let pick = Pick::default();
        assert!(!pick.checked());
        assert!(pick.on().contains("Use FlexRadio is on"));
        assert!(pick.checked());
        assert!(pick.take_changed());
        // On again changes nothing.
        pick.on();
        assert!(!pick.take_changed());
        assert_eq!(pick.current(), None);
        pick.chose("FREE-1");
        assert_eq!(pick.current().as_deref(), Some("FREE-1"));
        assert!(!pick.has_changed());
        // Lost with the checkmark on: it waits for a FlexRadio again.
        assert!(
            pick.lost()
                .is_some_and(|said| said.contains("waits for a FlexRadio again"))
        );
        assert!(pick.checked());
        assert_eq!(pick.current(), None);
        // Off: back to USB radios.
        assert!(pick.stop().contains("goes back to USB radios"));
        assert!(!pick.checked());
        assert!(pick.take_changed());
        assert!(pick.stop().contains("none in use"));
    }

    #[test]
    fn opens_smartsdr_once_for_the_programs_open() {
        let pick = Pick::default();
        assert!(!pick.open_smartsdr_now(false));
        assert!(pick.open_smartsdr_now(true));
        // Closed by someone while the programs stay open: left closed.
        assert!(!pick.open_smartsdr_now(true));
        // The programs closed; the next one to open gets it again.
        assert!(!pick.open_smartsdr_now(false));
        assert!(pick.open_smartsdr_now(true));
    }

    #[test]
    fn asks_which_flexradio_once_for_the_same_ones() {
        let pick = Pick::default();
        let free = ["A".to_owned(), "B".to_owned()];
        assert!(pick.ask_which(&free));
        assert!(!pick.ask_which(&free));
        assert!(pick.ask_which(&free[..1]));
        pick.on();
        assert!(pick.ask_which(&free[..1]));
    }

    #[test]
    fn keeps_a_flexradio_given_with_flex() {
        let pick = Pick::starting_with(Some("any"));
        assert_eq!(pick.current().as_deref(), Some("any"));
        assert!(!pick.has_changed());
        // Lost, it is looked for again, as a USB radio is.
        assert_eq!(pick.lost(), None);
        assert_eq!(pick.current().as_deref(), Some("any"));
        assert_eq!(Pick::starting_with(None).current(), None);
    }
}
