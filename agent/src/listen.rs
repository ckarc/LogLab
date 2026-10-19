// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Listening to FT8 and telling the Engine what was heard.
//!
//! Every 15-second slot's audio is drawn as a waterfall, sent to the Engine
//! a few rows every half second as it arrives, and decoded by LogLab's own FT8
//! decoder once the slot is over. The decodes go to the Engine as received
//! (Principle XX): each one came from the signal and passed the decoder's
//! checks. Nothing is guessed or filled in here.
//!
//! For now the audio comes only from the pretend FT8 band; a real radio's
//! audio comes later. Listening never keys a transmitter.

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use loglab_agent::ft8::bench;
use loglab_agent::ft8::decode::{self, Decode};
use loglab_agent::ft8::{SAMPLE_RATE, SLOT_SAMPLES, SLOT_SECONDS};
use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

use crate::engine::{Ft8Decode, Ft8Decodes, Ft8Report, Ft8Waterfall, Uplink};
use crate::pretend::FT8_DIAL_HZ;
use crate::pretend_ft8::PretendBand;

/// Waterfall rows per second of audio.
pub const ROWS_PER_SECOND: usize = 4;
/// Waterfall rows in one slot.
pub const ROWS_PER_SLOT: usize = ROWS_PER_SECOND * SLOT_SECONDS as usize;
/// Samples between waterfall rows.
const ROW_STEP: usize = SAMPLE_RATE as usize / ROWS_PER_SECOND;
/// FFT length for a waterfall row: 0.32 s, giving 3.125 Hz bins, half an
/// FT8 tone apart, so each tone of a signal shows on its own.
const ROW_FFT: usize = 3_840;
/// Waterfall columns, one FFT bin each: 0 to 3,000 Hz of audio.
pub const COLUMNS: usize = 960;
/// How far above the row's typical level is drawn brightest, in decibels.
const RANGE_DB: f32 = 25.0;
/// The level the typical (noise) column is drawn at, out of 255.
const FLOOR_LEVEL: f32 = 24.0;
/// How often the listener looks at the clock.
const TICK: Duration = Duration::from_millis(200);

/// Draws waterfall rows `from..to` of a slot's audio: each row is one hex
/// byte per column (00 dark to ff bright), relative to that row's typical
/// level, so static is dim and signals stand out.
#[must_use]
pub fn waterfall_rows(audio: &[f32], from: usize, to: usize) -> Vec<String> {
    let fft: Arc<dyn Fft<f32>> = FftPlanner::new().plan_fft_forward(ROW_FFT);
    let window: Vec<f32> = (0..ROW_FFT)
        .map(|i| {
            #[allow(clippy::cast_precision_loss)] // small whole numbers
            let x = i as f32 / ROW_FFT as f32;
            0.5 - 0.5 * (std::f32::consts::TAU * x).cos()
        })
        .collect();
    let mut buffer = vec![Complex32::new(0.0, 0.0); ROW_FFT];
    (from..to.min(ROWS_PER_SLOT))
        .map(|row| {
            let centre = row * ROW_STEP + ROW_STEP / 2;
            let start = centre.saturating_sub(ROW_FFT / 2);
            for (i, b) in buffer.iter_mut().enumerate() {
                let sample = audio.get(start + i).copied().unwrap_or(0.0);
                *b = Complex32::new(sample * window[i], 0.0);
            }
            fft.process(&mut buffer);
            let db: Vec<f32> = buffer
                .iter()
                .take(COLUMNS)
                .map(|bin| 10.0 * (bin.norm_sqr() + 1e-6).log10())
                .collect();
            let mut sorted = db.clone();
            sorted.sort_by(f32::total_cmp);
            let typical = sorted[sorted.len() / 2];
            let mut line = String::with_capacity(COLUMNS * 2);
            for level in db {
                let scaled = FLOOR_LEVEL + (level - typical) * (255.0 - FLOOR_LEVEL) / RANGE_DB;
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped
                let byte = scaled.round().clamp(0.0, 255.0) as u8;
                let _ = write!(line, "{byte:02x}");
            }
            line
        })
        .collect()
}

/// The pretend band's audio for slot `index`: every station's signal, with
/// static, as samples.
#[must_use]
pub fn pretend_audio(band: &mut PretendBand, index: u64) -> Vec<f32> {
    let slot = band.slot(index);
    match bench::render(&slot, index) {
        Ok(samples) => samples.into_iter().map(f32::from).collect(),
        // The pretend band only makes messages FT8 can send; if one ever
        // slips through, the slot is static alone.
        Err(_) => vec![0.0; SLOT_SAMPLES],
    }
}

fn decodes_out(decodes: Vec<Decode>) -> Vec<Ft8Decode> {
    decodes
        .into_iter()
        .map(|d| Ft8Decode {
            message: d.message,
            snr_db: d.snr_db,
            dt_s: d.dt_s,
            base_hz: d.base_hz,
        })
        .collect()
}

fn unix_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// A slot being listened to: its audio, and how many waterfall rows of it
/// the Engine has had.
struct Listening {
    index: u64,
    audio: Vec<f32>,
    rows_sent: usize,
}

/// Listen to the pretend FT8 band until `running` turns off, sending the
/// waterfall and every slot's decodes to the Engine through `uplink`.
pub fn listen_to_pretend_band(uplink: &Uplink, agent: &str, running: &AtomicBool, seed: u64) {
    let mut band = PretendBand::new(seed);
    let mut now_on: Option<Listening> = None;
    let mut decoding: Option<JoinHandle<(u64, Vec<Decode>)>> = None;
    let mut said_problem = false;
    let mut deliver = |report: &Ft8Report<'_>| match uplink.send_ft8(report) {
        Ok(()) => said_problem = false,
        Err(error) => {
            if !said_problem {
                println!("FT8: {error}");
            }
            said_problem = true;
        }
    };
    while running.load(Ordering::SeqCst) {
        let now = unix_seconds();
        let slot_seconds = f64::from(SLOT_SECONDS);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // after 1970
        let index = (now / slot_seconds).floor() as u64;
        #[allow(clippy::cast_precision_loss)] // slot numbers stay far below 2^52
        let into = now - index as f64 * slot_seconds;

        if now_on.as_ref().is_none_or(|l| l.index != index) {
            // A slot just ended: decode it while the next one plays.
            if let Some(ended) = now_on.take()
                && decoding.is_none()
            {
                decoding = Some(thread::spawn(move || {
                    let found = decode::decode_slot(&ended.audio, &decode::Options::default());
                    (ended.index, found)
                }));
            }
            now_on = Some(Listening {
                index,
                audio: pretend_audio(&mut band, index),
                rows_sent: 0,
            });
        }

        if let Some(listening) = now_on.as_mut() {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss
            )] // 0 to 60
            let ready = ((into * ROWS_PER_SECOND as f64).floor() as usize).min(ROWS_PER_SLOT);
            // Half a second of rows at a time.
            if ready >= listening.rows_sent + ROWS_PER_SECOND / 2
                || (ready == ROWS_PER_SLOT && ready > listening.rows_sent)
            {
                let rows = waterfall_rows(&listening.audio, listening.rows_sent, ready);
                deliver(&Ft8Report {
                    agent,
                    source: "pretend",
                    dial_hz: FT8_DIAL_HZ,
                    waterfall: Some(Ft8Waterfall {
                        slot: listening.index,
                        first_row: listening.rows_sent,
                        rows,
                    }),
                    decodes: None,
                });
                listening.rows_sent = ready;
            }
        }

        if decoding.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(done) = decoding.take()
            && let Ok((slot, found)) = done.join()
        {
            deliver(&Ft8Report {
                agent,
                source: "pretend",
                dial_hz: FT8_DIAL_HZ,
                waterfall: None,
                decodes: Some(Ft8Decodes {
                    slot,
                    decodes: decodes_out(found),
                }),
            });
        }
        thread::sleep(TICK);
    }
}

#[cfg(test)]
mod tests {
    use loglab_agent::ft8::decode;

    use super::{COLUMNS, ROWS_PER_SLOT, pretend_audio, waterfall_rows};
    use crate::pretend_ft8::PretendBand;

    #[test]
    fn the_decoder_hears_the_pretend_band() {
        let mut band = PretendBand::new(21);
        // The first slots set the band going; listen to a later one.
        for index in 0..6 {
            let _ = band.slot(index);
        }
        let mut copy = band.clone();
        let sent = copy.slot(6).signals;
        let audio = pretend_audio(&mut band, 6);
        let heard = decode::decode_slot(&audio, &decode::Options::default());
        // Everything heard was really sent: the pretend band never makes the
        // decoder report a message that is not there.
        for d in &heard {
            assert!(
                sent.iter().any(|s| s.message == d.message),
                "{} was decoded but never sent",
                d.message
            );
        }
        // And it hears most of the stronger stations.
        let strong = sent.iter().filter(|s| s.snr_db > -15.0).count();
        assert!(
            heard.len() * 10 >= strong * 8,
            "heard {} of {strong} strong signals",
            heard.len()
        );
    }

    #[test]
    fn the_waterfall_shows_where_the_signals_are() {
        let mut band = PretendBand::new(4);
        for index in 0..4 {
            let _ = band.slot(index);
        }
        let mut copy = band.clone();
        let sent = copy.slot(4).signals;
        let audio = pretend_audio(&mut band, 4);
        let rows = waterfall_rows(&audio, 0, ROWS_PER_SLOT);
        assert_eq!(rows.len(), ROWS_PER_SLOT);
        assert!(rows.iter().all(|r| r.len() == COLUMNS * 2));
        // In the middle of the slot, the brightest column sits on a signal.
        let row = &rows[ROWS_PER_SLOT / 2];
        let levels: Vec<u8> = (0..COLUMNS)
            .map(|c| u8::from_str_radix(&row[c * 2..c * 2 + 2], 16).unwrap_or(0))
            .collect();
        let brightest = (0..COLUMNS).max_by_key(|&c| levels[c]).unwrap_or(0);
        #[allow(clippy::cast_precision_loss)] // small whole numbers
        let hz = brightest as f64 * 3.125;
        assert!(
            sent.iter()
                .any(|s| hz >= s.base_hz - 10.0 && hz <= s.base_hz + 60.0),
            "the brightest spot, {hz} Hz, is on no signal"
        );
    }

    #[test]
    fn rows_past_the_slot_are_not_drawn() {
        let audio = vec![0.0; 180_000];
        assert_eq!(waterfall_rows(&audio, ROWS_PER_SLOT - 2, 80).len(), 2);
        assert!(waterfall_rows(&audio, ROWS_PER_SLOT, 80).is_empty());
    }
}
