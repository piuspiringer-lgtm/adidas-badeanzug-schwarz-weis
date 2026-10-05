//! Mikrofonaufnahme für Push-to-Talk (nur auf ausdrückliche Benutzeraktion).
//!
//! Der Audio-Stream lebt in einem eigenen Thread (CoreAudio-Streams sind
//! nicht `Send`). `stop()` beendet die Aufnahme und liefert 16-kHz-Mono-PCM,
//! das als WAV an whisper.cpp geht. Die Aufnahme ist auf 60 s begrenzt.

use crate::VoiceError;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::path::Path;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const TARGET_RATE: u32 = 16_000;
pub const MAX_SECONDS: u64 = 60;

/// Laufende Aufnahme. Drop ohne `stop()` verwirft die Aufnahme.
pub struct Recording {
    stop_tx: mpsc::Sender<()>,
    thread: Option<JoinHandle<Result<Vec<f32>, VoiceError>>>,
    started: Instant,
}

impl Recording {
    /// Startet die Aufnahme vom Standard-Mikrofon.
    pub fn start() -> Result<Self, VoiceError> {
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), VoiceError>>();
        let thread = std::thread::spawn(move || -> Result<Vec<f32>, VoiceError> {
            let host = cpal::default_host();
            let fail = |m: String| VoiceError::Process(m);
            let setup = (|| {
                let device = host.default_input_device().ok_or_else(|| VoiceError::NotInstalled("kein Mikrofon gefunden".into()))?;
                let config = device.default_input_config().map_err(|e| fail(e.to_string()))?;
                Ok::<_, VoiceError>((device, config))
            })();
            let (device, config) = match setup {
                Ok(v) => v,
                Err(e) => {
                    let _ = ready_tx.send(Err(VoiceError::Process(e.to_string())));
                    return Err(e);
                }
            };
            let rate = config.sample_rate().0;
            let channels = config.channels() as usize;
            let samples: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::with_capacity(rate as usize * 10)));
            let max = rate as usize * channels * MAX_SECONDS as usize;
            let sink = samples.clone();
            let err_fn = |e| eprintln!("[jarvis-voice] Audiofehler: {e}");
            let stream = match config.sample_format() {
                cpal::SampleFormat::F32 => device.build_input_stream(
                    &config.into(),
                    move |data: &[f32], _: &_| {
                        let mut s = sink.lock().unwrap();
                        if s.len() < max {
                            s.extend_from_slice(data);
                        }
                    },
                    err_fn,
                    None,
                ),
                cpal::SampleFormat::I16 => device.build_input_stream(
                    &config.into(),
                    move |data: &[i16], _: &_| {
                        let mut s = sink.lock().unwrap();
                        if s.len() < max {
                            s.extend(data.iter().map(|v| *v as f32 / i16::MAX as f32));
                        }
                    },
                    err_fn,
                    None,
                ),
                other => {
                    let e = fail(format!("Sample-Format {other:?} nicht unterstützt"));
                    let _ = ready_tx.send(Err(VoiceError::Process(e.to_string())));
                    return Err(e);
                }
            };
            let started = stream.map_err(|e| e.to_string()).and_then(|s| s.play().map(|_| s).map_err(|e| e.to_string()));
            let stream = match started {
                Ok(s) => s,
                Err(e) => {
                    let msg = format!("Mikrofon nicht verfügbar (Systemeinstellungen → Datenschutz → Mikrofon?): {e}");
                    let _ = ready_tx.send(Err(VoiceError::Process(msg.clone())));
                    return Err(VoiceError::Process(msg));
                }
            };
            let _ = ready_tx.send(Ok(()));
            // Warten auf Stop oder Zeitlimit.
            let _ = stop_rx.recv_timeout(Duration::from_secs(MAX_SECONDS));
            drop(stream);
            let raw = std::mem::take(&mut *samples.lock().unwrap());
            Ok(resample_mono(&raw, channels, rate, TARGET_RATE))
        });
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Self { stop_tx, thread: Some(thread), started: Instant::now() }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(VoiceError::Timeout),
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Beendet die Aufnahme und liefert 16-kHz-Mono-Samples.
    pub fn stop(mut self) -> Result<Vec<f32>, VoiceError> {
        let _ = self.stop_tx.send(());
        self.thread.take().unwrap().join().map_err(|_| VoiceError::Process("Aufnahme-Thread abgestürzt".into()))?
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        let _ = self.stop_tx.send(());
    }
}

/// Kanäle mitteln und linear auf `to` Hz umrechnen.
pub fn resample_mono(interleaved: &[f32], channels: usize, from: u32, to: u32) -> Vec<f32> {
    let channels = channels.max(1);
    let mono: Vec<f32> = interleaved.chunks(channels).map(|f| f.iter().sum::<f32>() / f.len() as f32).collect();
    if from == to || mono.is_empty() {
        return mono;
    }
    let ratio = from as f64 / to as f64;
    let n = ((mono.len() as f64) / ratio).floor() as usize;
    (0..n)
        .map(|i| {
            let pos = i as f64 * ratio;
            let j = pos.floor() as usize;
            let frac = (pos - j as f64) as f32;
            let a = mono[j];
            let b = *mono.get(j + 1).unwrap_or(&a);
            a + (b - a) * frac
        })
        .collect()
}

/// Mittlere Lautstärke (RMS) – erkennt "nichts gesagt".
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

/// Schreibt 16-bit-PCM-WAV (mono).
pub fn write_wav(path: &Path, samples: &[f32], rate: u32) -> std::io::Result<()> {
    let data_len = (samples.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + data_len as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes()); // PCM
    b.extend_from_slice(&1u16.to_le_bytes()); // mono
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        b.extend_from_slice(&((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).to_le_bytes());
    }
    std::fs::write(path, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampling_and_mixdown() {
        // 48 kHz Stereo, 0,1 s → 16 kHz Mono, 1600 Samples
        let stereo: Vec<f32> = (0..4800).flat_map(|i| [i as f32 / 4800.0, -(i as f32) / 4800.0]).collect();
        let out = resample_mono(&stereo, 2, 48_000, 16_000);
        assert_eq!(out.len(), 1600);
        assert!(out.iter().all(|v| v.abs() < 1e-6), "Kanäle gemittelt");
        let mono: Vec<f32> = (0..441).map(|i| i as f32 / 441.0).collect();
        let r = resample_mono(&mono, 1, 44_100, 16_000);
        assert_eq!(r.len(), 160);
        assert!(r.windows(2).all(|w| w[1] >= w[0]), "monoton wie das Original");
    }

    #[test]
    fn wav_header_and_silence_detection() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.wav");
        let samples = vec![0.5f32; 16_000];
        write_wav(&p, &samples, 16_000).unwrap();
        let b = std::fs::read(&p).unwrap();
        assert_eq!(&b[0..4], b"RIFF");
        assert_eq!(&b[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), 16_000);
        assert_eq!(b.len(), 44 + 32_000);
        assert!(rms(&samples) > 0.4);
        assert!(rms(&vec![0.0; 100]) < 1e-6);
    }
}
