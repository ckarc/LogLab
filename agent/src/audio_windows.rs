// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Receiving a radio's audio through the Agent, on Windows.
//!
//! The Agent captures the radio's USB sound device and plays it into a
//! VB-Audio virtual cable, so a program such as WSJT-X takes its audio from
//! the cable ("CABLE Output") and never opens the radio's sound device
//! itself. [`crate::audio`] keeps the two devices' clocks in step.
//!
//! **Receive only.** Audio played into a radio can key it (through VOX, or
//! data mode with USB audio), so this module never plays anything to a
//! radio: the device it plays into must be a VB-Audio virtual cable, and it
//! refuses every other one. Sending a program's audio to the radio comes
//! with transmitting, for the program the person at the computer checks in
//! the tray program.
//!
//! The sound devices are reached through cpal (Apache-2.0), which uses
//! Windows' own audio system (WASAPI).

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

use crate::audio::{Buffer, Resampler};

/// What a virtual cable's playing side is named: "CABLE Input
/// (VB-Audio Virtual Cable)", "CABLE-A Input (VB-Audio Cable A)" and so on.
const CABLE: &str = "VB-Audio";
/// How often the status line is printed.
const STATUS_EVERY: Duration = Duration::from_secs(2);
/// Peaks louder than this are close to clipping.
const TOO_LOUD_DBFS: f64 = -1.0;
/// Peaks quieter than this are hard to decode.
const TOO_QUIET_DBFS: f64 = -50.0;

/// One sound device, as `audio-devices` lists it.
pub struct Listed {
    /// Its name, as Windows shows it.
    pub name: String,
    /// Its usual format, such as "48000 Hz, 2 channels, f32".
    pub format: String,
}

/// Every capturing and every playing sound device.
///
/// # Errors
///
/// Windows' audio system couldn't be asked.
pub fn devices() -> Result<(Vec<Listed>, Vec<Listed>), String> {
    let host = cpal::default_host();
    let fail = |error: cpal::Error| format!("can't list sound devices: {error}");
    let inputs = host
        .input_devices()
        .map_err(fail)?
        .map(|device| Listed {
            name: name_of(&device),
            format: device
                .default_input_config()
                .map_or_else(|error| format!("(no format: {error})"), |c| describe(&c)),
        })
        .collect();
    let outputs = host
        .output_devices()
        .map_err(fail)?
        .map(|device| Listed {
            name: name_of(&device),
            format: device
                .default_output_config()
                .map_or_else(|error| format!("(no format: {error})"), |c| describe(&c)),
        })
        .collect();
    Ok((inputs, outputs))
}

fn name_of(device: &cpal::Device) -> String {
    device
        .description()
        .map_or_else(|_| "(unnamed device)".to_owned(), |d| d.name().to_owned())
}

fn describe(config: &cpal::SupportedStreamConfig) -> String {
    format!(
        "{} Hz, {} channel{}, {:?}",
        config.sample_rate(),
        config.channels(),
        if config.channels() == 1 { "" } else { "s" },
        config.sample_format()
    )
    .to_lowercase()
    .replace(" hz", " Hz")
}

/// Finds the one device whose name contains `wanted`, ignoring case.
fn find(
    devices: impl Iterator<Item = cpal::Device>,
    wanted: &str,
    kind: &str,
) -> Result<(cpal::Device, String), String> {
    let wanted_lower = wanted.to_lowercase();
    let mut found: Vec<(cpal::Device, String)> = devices
        .map(|device| {
            let name = name_of(&device);
            (device, name)
        })
        .filter(|(_, name)| name.to_lowercase().contains(&wanted_lower))
        .collect();
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err(format!(
            "no {kind} sound device has {wanted:?} in its name; `loglab-agent audio-devices` lists them"
        )),
        _ => Err(format!(
            "{} {kind} sound devices have {wanted:?} in their names, so give more of the name: {}",
            found.len(),
            found
                .iter()
                .map(|(_, name)| format!("{name:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Plays the radio's audio, from the capturing device whose name contains
/// `from`, into the virtual cable whose name contains `to`, until `running`
/// turns false. Prints a status line every few seconds.
///
/// # Errors
///
/// A device isn't found, more than one matches, the playing device isn't a
/// VB-Audio virtual cable, or a device can't be opened.
pub fn receive(
    from: &str,
    to: &str,
    running: &std::sync::atomic::AtomicBool,
) -> Result<(), String> {
    let host = cpal::default_host();
    let (radio, radio_name) = find(
        host.input_devices()
            .map_err(|error| format!("can't list sound devices: {error}"))?,
        from,
        "capturing",
    )?;
    if radio_name.contains(CABLE) {
        return Err(format!(
            "{radio_name:?} is a virtual cable, not a radio: --from names the radio's sound device"
        ));
    }
    let (cable, cable_name) = find(
        host.output_devices()
            .map_err(|error| format!("can't list sound devices: {error}"))?,
        to,
        "playing",
    )?;
    if !cable_name.contains(CABLE) {
        return Err(format!(
            "{cable_name:?} is not a VB-Audio virtual cable. The Agent only receives for now: it plays the radio's audio into a virtual cable, never into a radio, speaker or other device, because audio played into a radio can key it."
        ));
    }
    let radio_config = radio
        .default_input_config()
        .map_err(|error| format!("can't read {radio_name:?}'s format: {error}"))?;
    let cable_config = cable
        .default_output_config()
        .map_err(|error| format!("can't read {cable_name:?}'s format: {error}"))?;

    let buffer = Buffer::new(radio_config.sample_rate());
    let (errors, heard) = mpsc::channel();
    let capture = open_capture(&radio, &radio_config, Arc::clone(&buffer), errors.clone())?;
    let playback = open_playback(&cable, &cable_config, Arc::clone(&buffer), errors)?;
    capture
        .play()
        .map_err(|error| format!("can't start {radio_name:?}: {error}"))?;
    playback
        .play()
        .map_err(|error| format!("can't start {cable_name:?}: {error}"))?;

    println!("Receiving: {radio_name} ({})", describe(&radio_config));
    println!("      into {cable_name} ({})", describe(&cable_config));
    println!("In WSJT-X: Settings > Audio > Input: the cable's recording side, \"CABLE Output\".");
    println!("Receive only: nothing is played to the radio. Ctrl-C stops.");
    watch(&buffer, &heard, running);
    drop(playback);
    drop(capture);
    println!("stopped");
    Ok(())
}

/// Prints how the audio is doing until `running` turns false.
fn watch(buffer: &Buffer, heard: &Receiver<String>, running: &std::sync::atomic::AtomicBool) {
    let mut last = Instant::now();
    let mut gaps = 0;
    let mut dropped = 0;
    while running.load(std::sync::atomic::Ordering::SeqCst) {
        for error in heard.try_iter() {
            println!("{}  sound device error: {error}", crate::utc_clock());
        }
        if last.elapsed() >= STATUS_EVERY {
            last = Instant::now();
            let peak = buffer.take_peak_dbfs();
            let new_gaps = buffer.gaps() - gaps;
            gaps = buffer.gaps();
            let new_dropped = buffer.dropped() - dropped;
            dropped = buffer.dropped();
            println!(
                "{}  radio audio {} | buffer {} ms | {}",
                crate::utc_clock(),
                peak.map_or_else(|| "silent".to_owned(), |db| format!("{db:>4.0} dBFS peak")),
                buffer.level().as_millis(),
                match new_gaps {
                    0 => "no gaps".to_owned(),
                    1 => "1 gap".to_owned(),
                    n => format!("{n} gaps"),
                }
            );
            if let Some(hint) = level_hint(peak) {
                println!("           -> {hint}");
            }
            if new_dropped > 0 {
                println!(
                    "           -> {new_dropped} samples thrown away: nothing is taking audio from the cable side fast enough. Is the cable's playing side muted or stalled?"
                );
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// What to do about a level that's too loud or too quiet.
fn level_hint(peak: Option<f64>) -> Option<&'static str> {
    match peak {
        None => Some(
            "No audio from the radio. On an IC-7300: ACC/USB Output Select should be AF, and ACC/USB AF Output Level above 0.",
        ),
        Some(db) if db > TOO_LOUD_DBFS => Some(
            "Close to clipping. Turn down the radio's USB audio level (IC-7300: ACC/USB AF Output Level).",
        ),
        Some(db) if db < TOO_QUIET_DBFS => Some(
            "Very quiet. Turn up the radio's USB audio level (IC-7300: ACC/USB AF Output Level), or check the band has signals.",
        ),
        Some(_) => None,
    }
}

fn open_capture(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    buffer: Arc<Buffer>,
    errors: mpsc::Sender<String>,
) -> Result<cpal::Stream, String> {
    let channels = usize::from(config.channels());
    let stream_config = config.config();
    let report = move |error: cpal::Error| {
        let _ = errors.send(error.to_string());
    };
    let built = match config.sample_format() {
        SampleFormat::F32 => capture_as::<f32>(device, stream_config, channels, buffer, report),
        SampleFormat::I16 => capture_as::<i16>(device, stream_config, channels, buffer, report),
        SampleFormat::I32 => capture_as::<i32>(device, stream_config, channels, buffer, report),
        SampleFormat::U16 => capture_as::<u16>(device, stream_config, channels, buffer, report),
        other => {
            return Err(format!(
                "the radio's sound device uses {other:?} samples, which the Agent can't read yet"
            ));
        }
    };
    built.map_err(|error| format!("can't open the radio's sound device: {error}"))
}

fn capture_as<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    buffer: Arc<Buffer>,
    report: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    device.build_input_stream::<T, _, _>(
        config,
        move |data: &[T], _| {
            let channels = channels.max(1);
            buffer.push(data.chunks(channels).map(|frame| {
                // A frame holds at most a u16's worth of channels.
                let width = f32::from(u16::try_from(frame.len()).unwrap_or(u16::MAX));
                frame.iter().map(|s| f32::from_sample_(*s)).sum::<f32>() / width
            }));
        },
        report,
        None,
    )
}

fn open_playback(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    buffer: Arc<Buffer>,
    errors: mpsc::Sender<String>,
) -> Result<cpal::Stream, String> {
    let channels = usize::from(config.channels());
    let resampler = Resampler::new(buffer, config.sample_rate());
    let stream_config = config.config();
    let report = move |error: cpal::Error| {
        let _ = errors.send(error.to_string());
    };
    let built = match config.sample_format() {
        SampleFormat::F32 => play_as::<f32>(device, stream_config, channels, resampler, report),
        SampleFormat::I16 => play_as::<i16>(device, stream_config, channels, resampler, report),
        SampleFormat::I32 => play_as::<i32>(device, stream_config, channels, resampler, report),
        SampleFormat::U16 => play_as::<u16>(device, stream_config, channels, resampler, report),
        other => {
            return Err(format!(
                "the virtual cable uses {other:?} samples, which the Agent can't write yet"
            ));
        }
    };
    built.map_err(|error| format!("can't open the virtual cable: {error}"))
}

fn play_as<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    mut resampler: Resampler,
    report: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    let mut mono = Vec::new();
    device.build_output_stream::<T, _, _>(
        config,
        move |data: &mut [T], _| {
            let channels = channels.max(1);
            mono.resize(data.len() / channels, 0.0);
            resampler.fill(&mut mono);
            for (frame, sample) in data.chunks_mut(channels).zip(&mono) {
                let value = T::from_sample_(*sample);
                frame.fill(value);
            }
        },
        report,
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::level_hint;

    #[test]
    fn says_what_to_do_about_the_level() {
        assert!(level_hint(None).is_some_and(|h| h.contains("No audio")));
        assert!(level_hint(Some(-0.2)).is_some_and(|h| h.contains("clipping")));
        assert!(level_hint(Some(-70.0)).is_some_and(|h| h.contains("quiet")));
        assert_eq!(level_hint(Some(-20.0)), None);
    }
}
