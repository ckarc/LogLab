// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! `SoundCardTest` on Windows (see `sound_card`): serves "LabLog Radio" over
//! USB/IP, has usbip-win2's `usbip.exe` plug it in, waits for Windows to
//! show its output and input, plays the test tone to the output through
//! Windows' own audio system (WASAPI, through `cpal`) while recording the
//! input, and checks the tone came back whole. Then it unplugs the card.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use usbip_audio::uac2::Uac2Device;
use usbip_audio::{MonotonicClock, Server, usbip_exe};

use crate::sound_card::{Loopback, NAME, RATE, analyse, config, judge, tone};

/// How long Windows may take to show the card's output and input.
const APPEAR_WITHIN: Duration = Duration::from_mins(1);
/// The first second recorded is left out: the streams are still starting.
const SETTLE: Duration = Duration::from_secs(1);

/// Unplugs the card when dropped, whatever happened.
struct Plugged {
    usbip: std::path::PathBuf,
    port: u32,
}

impl Drop for Plugged {
    fn drop(&mut self) {
        match usbip_exe::detach(&self.usbip, self.port) {
            Ok(()) => println!("unplugged {NAME} (usbip port {})", self.port),
            Err(why) => println!("couldn't unplug {NAME}: {why}"),
        }
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
            .and_then(|mut devices| devices.find(|d| name_of(d).contains(NAME)));
        let input = host
            .input_devices()
            .ok()
            .and_then(|mut devices| devices.find(|d| name_of(d).contains(NAME)));
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
    heard: Arc<Mutex<Vec<f32>>>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels).max(1);
    device
        .build_input_stream::<T, _, _>(
            config,
            move |data: &[T], _| {
                if let Ok(mut heard) = heard.lock() {
                    heard.extend(data.chunks(channels).map(|f| f32::from_sample_(f[0])));
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

fn record(device: &cpal::Device, heard: Arc<Mutex<Vec<f32>>>) -> Result<cpal::Stream, String> {
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
    let card = Uac2Device::new(config(), Arc::clone(&loopback))?;
    let server = Server::start(0, Arc::new(MonotonicClock::new()))
        .map_err(|error| format!("can't serve {NAME} over USB/IP: {error}"))?;
    server.add("1-1", Arc::new(card));
    println!("serving {NAME} over USB/IP on 127.0.0.1:{}", server.port());
    let usbip =
        usbip_exe::find().ok_or("usbip-win2 isn't installed: its usbip.exe wasn't found")?;
    let port = usbip_exe::attach(&usbip, server.port(), "1-1")?;
    let _plugged = Plugged { usbip, port };
    println!("plugged {NAME} in (usbip port {port})");

    let host = cpal::default_host();
    let (output, input) = wait_for_card(&host)?;
    let heard = Arc::new(Mutex::new(Vec::new()));
    let recording = record(&input, Arc::clone(&heard))?;
    let playing = play(&output)?;
    recording
        .play()
        .map_err(|error| format!("can't start recording: {error}"))?;
    playing
        .play()
        .map_err(|error| format!("can't start playing: {error}"))?;
    std::thread::sleep(Duration::from_secs(seconds));
    drop(playing);
    drop(recording);

    let counts = loopback.counts();
    println!(
        "the card carried {} frames played, {} recorded, {} short, {} dropped",
        counts.played, counts.recorded, counts.short, counts.dropped
    );
    let heard = heard.lock().map(|h| h.clone()).unwrap_or_default();
    let settle = usize::try_from(SETTLE.as_secs()).unwrap_or(1) * RATE as usize;
    // Leave out the start, and the end, where playing stopped first.
    let end = heard.len().saturating_sub(RATE as usize / 2);
    let kept = heard.get(settle..end).unwrap_or_default();
    let result = analyse(kept);
    let kept_seconds = f64::from(u32::try_from(kept.len()).unwrap_or(u32::MAX)) / f64::from(RATE);
    println!(
        "heard {:.1} Hz at {:.2} dBFS over {:.1} s",
        result.hz, result.peak_dbfs, kept_seconds
    );
    judge(&result)
}
