// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! "LabLog Radio" on Windows (see `sound_card`): [`Card`] serves it over
//! USB/IP and has usbip-win2's `usbip.exe` plug it in, and unplugs it when
//! dropped. `FlexTest` keeps one plugged in while it runs.
//!
//! `SoundCardTest` plugs one in, waits for Windows to show its output and
//! input, plays the test tone to the output through Windows' own audio
//! system (WASAPI, through `cpal`) while recording the input, and checks
//! the tone came back whole and the input is a line input.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use usbip_audio::uac2::{Uac2Device, Uac2Streams};
use usbip_audio::{MonotonicClock, Server, usbip_exe};

use crate::sound_card::{LineIn, Listener, Loopback, NAME, RATE, config, is_card, judge, tone};

/// How long Windows may take to show the card's output and input.
const APPEAR_WITHIN: Duration = Duration::from_mins(1);
/// The first second recorded is left out: the streams are still starting.
const SETTLE_FRAMES: u64 = RATE as u64;
/// How often a long run says how it's going.
const STATUS_EVERY: Duration = Duration::from_mins(1);
/// Microsoft Learn, "Process Creation Flags": no console window flashes up
/// when a tray program runs `usbip.exe`.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// The card's place on its USB/IP server.
const BUS_ID: &str = "1-1";

/// Runs `usbip.exe` with `args`, with no console window; its output.
fn usbip(exe: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new(exe)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| format!("can't run {}: {error}", exe.display()))?;
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if output.status.success() {
        Ok(said)
    } else {
        Err(format!("usbip.exe {}: {}", args.join(" "), said.trim()))
    }
}

/// "LabLog Radio", plugged in to this computer. Dropping it unplugs it.
pub struct Card {
    usbip: PathBuf,
    port: Option<u32>,
    // Dropped after `drop` has unplugged the card.
    _server: Server,
}

impl Card {
    /// Plugs in the card, carrying `streams`.
    ///
    /// # Errors
    ///
    /// usbip-win2 isn't installed, or wouldn't plug the card in.
    pub fn plug_in<S: Uac2Streams + 'static>(streams: S) -> Result<Self, String> {
        let card = LineIn(Uac2Device::new(config(), streams)?);
        let usbip_exe =
            usbip_exe::find().ok_or("usbip-win2 isn't installed: its usbip.exe wasn't found")?;
        let server = Server::start(0, Arc::new(MonotonicClock::new()))
            .map_err(|error| format!("can't serve {NAME} over USB/IP: {error}"))?;
        server.add(BUS_ID, Arc::new(card));
        // As usbip-audio's own `usbip_exe::attach`, without a console window.
        let said = usbip(
            &usbip_exe,
            &[
                "--tcp-port",
                &server.port().to_string(),
                "attach",
                "--remote",
                "127.0.0.1",
                "--bus-id",
                BUS_ID,
                "--once",
                "--terse",
            ],
        )?;
        let port = said
            .trim()
            .parse()
            .map_err(|_| format!("usbip.exe attach gave no port: {:?}", said.trim()))?;
        Ok(Self {
            usbip: usbip_exe,
            port: Some(port),
            _server: server,
        })
    }

    /// The usbip-win2 port it is plugged in to.
    #[must_use]
    pub fn port(&self) -> Option<u32> {
        self.port
    }

    /// Unplugs it now, saying whether that worked.
    ///
    /// # Errors
    ///
    /// `usbip.exe` wouldn't unplug it.
    pub fn unplug(&mut self) -> Result<(), String> {
        match self.port.take() {
            Some(port) => usbip(&self.usbip, &["detach", "--port", &port.to_string()]).map(|_| ()),
            None => Ok(()),
        }
    }
}

impl Drop for Card {
    fn drop(&mut self) {
        // Nowhere to say it failed; usbip-win2 also unplugs all at shutdown.
        let _ = self.unplug();
    }
}

fn name_of(device: &cpal::Device) -> String {
    device
        .description()
        .map_or_else(|_| "(unnamed device)".to_owned(), |d| d.name().to_owned())
}

/// The card's output and input, once Windows shows both.
fn wait_for_card(host: &cpal::Host) -> Result<(cpal::Device, cpal::Device), String> {
    let started = Instant::now();
    loop {
        let output = host
            .output_devices()
            .ok()
            .and_then(|mut devices| devices.find(|d| is_card(&name_of(d))));
        let input = host
            .input_devices()
            .ok()
            .and_then(|mut devices| devices.find(|d| is_card(&name_of(d))));
        if let (Some(output), Some(input)) = (output, input) {
            println!(
                "Windows shows {:?} after {:.1} s",
                name_of(&output),
                started.elapsed().as_secs_f64()
            );
            println!("and {:?}", name_of(&input));
            return Ok((output, input));
        }
        if started.elapsed() > APPEAR_WITHIN {
            return Err(format!(
                "Windows didn't show {NAME}'s output and input within {} s",
                APPEAR_WITHIN.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn play_as<T>(device: &cpal::Device, config: cpal::StreamConfig) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels).max(1);
    let mut frame: u64 = 0;
    device
        .build_output_stream::<T, _, _>(
            config,
            move |data: &mut [T], _| {
                for out in data.chunks_mut(channels) {
                    out.fill(T::from_sample_(tone(frame)));
                    frame += 1;
                }
            },
            |error| println!("playing: {error}"),
            None,
        )
        .map_err(|error| format!("can't play to {NAME}: {error}"))
}

fn record_as<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    heard: Arc<Mutex<Listener>>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels).max(1);
    let mut frames: u64 = 0;
    device
        .build_input_stream::<T, _, _>(
            config,
            move |data: &[T], _| {
                let Ok(mut heard) = heard.lock() else {
                    return;
                };
                for frame in data.chunks(channels) {
                    frames += 1;
                    if frames > SETTLE_FRAMES {
                        heard.hear(f32::from_sample_(frame[0]));
                    }
                }
            },
            |error| println!("recording: {error}"),
            None,
        )
        .map_err(|error| format!("can't record from {NAME}: {error}"))
}

fn play(device: &cpal::Device) -> Result<cpal::Stream, String> {
    let supported = device
        .default_output_config()
        .map_err(|error| format!("{NAME}'s output has no format: {error}"))?;
    println!(
        "playing at {} Hz, {} channels, {:?}",
        supported.sample_rate(),
        supported.channels(),
        supported.sample_format()
    );
    let config = supported.config();
    match supported.sample_format() {
        SampleFormat::F32 => play_as::<f32>(device, config),
        SampleFormat::I16 => play_as::<i16>(device, config),
        SampleFormat::I32 => play_as::<i32>(device, config),
        other => Err(format!("{NAME}'s output uses {other:?} samples")),
    }
}

fn record(device: &cpal::Device, heard: Arc<Mutex<Listener>>) -> Result<cpal::Stream, String> {
    let supported = device
        .default_input_config()
        .map_err(|error| format!("{NAME}'s input has no format: {error}"))?;
    println!(
        "recording at {} Hz, {} channels, {:?}",
        supported.sample_rate(),
        supported.channels(),
        supported.sample_format()
    );
    if supported.sample_rate() != RATE {
        return Err(format!(
            "Windows records {NAME} at {} Hz, not {RATE} Hz",
            supported.sample_rate()
        ));
    }
    let config = supported.config();
    match supported.sample_format() {
        SampleFormat::F32 => record_as::<f32>(device, config, heard),
        SampleFormat::I16 => record_as::<i16>(device, config, heard),
        SampleFormat::I32 => record_as::<i32>(device, config, heard),
        other => Err(format!("{NAME}'s input uses {other:?} samples")),
    }
}

/// Plugs the card in, plays the tone through it for `seconds`, checks it.
///
/// # Errors
///
/// usbip-win2 isn't installed, the card didn't appear, or the tone didn't
/// come back whole.
pub fn run(seconds: u64) -> Result<(), String> {
    let loopback = Arc::new(Loopback::default());
    let mut card = Card::plug_in(Arc::clone(&loopback))?;
    println!(
        "plugged {NAME} in (usbip port {})",
        card.port().unwrap_or_default()
    );

    let host = cpal::default_host();
    let (output, input) = wait_for_card(&host)?;
    // That the input is a line input, so Windows' Voice Clarity leaves it
    // alone, is in what the card tells Windows (`sound_card::as_line_in`,
    // tested there); the tone coming back whole below shows nothing
    // filtered it.
    let heard = Arc::new(Mutex::new(Listener::default()));
    let recording = record(&input, Arc::clone(&heard))?;
    let playing = play(&output)?;
    recording
        .play()
        .map_err(|error| format!("can't start recording: {error}"))?;
    playing
        .play()
        .map_err(|error| format!("can't start playing: {error}"))?;
    let started = Instant::now();
    let run_for = Duration::from_secs(seconds);
    while started.elapsed() < run_for {
        std::thread::sleep(STATUS_EVERY.min(run_for.saturating_sub(started.elapsed())));
        let counts = loopback.counts();
        let so_far = heard.lock().map(|h| h.heard()).ok();
        println!(
            "{:>6.0} s: played {}, recorded {}, held {} frames, short {}, dropped {}{}",
            started.elapsed().as_secs_f64(),
            counts.played,
            counts.recorded,
            loopback.held_frames(),
            counts.short,
            counts.dropped,
            so_far.map_or_else(String::new, |h| format!(
                "; heard {:.1} Hz at {:.2} dBFS, {} dropouts",
                h.hz, h.peak_dbfs, h.gaps
            ))
        );
    }
    // Recording stops first, so no silence after the tone is heard.
    drop(recording);
    drop(playing);
    match card.unplug() {
        Ok(()) => println!("unplugged {NAME}"),
        Err(why) => println!("couldn't unplug {NAME}: {why}"),
    }

    let counts = loopback.counts();
    println!(
        "the card carried {} frames played, {} recorded, {} short, {} dropped",
        counts.played, counts.recorded, counts.short, counts.dropped
    );
    let result = heard
        .lock()
        .map(|h| h.heard())
        .map_err(|_| "the recording was lost")?;
    // Sample counts within a test run fit an f64's 53 bits exactly.
    #[allow(clippy::cast_precision_loss)]
    let heard_seconds = result.samples as f64 / f64::from(RATE);
    println!(
        "heard {:.1} Hz at {:.2} dBFS over {heard_seconds:.1} s, {} dropouts",
        result.hz, result.peak_dbfs, result.gaps
    );
    judge(&result)
}
