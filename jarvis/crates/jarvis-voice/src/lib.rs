//! Voice: lokale Spracherkennung (whisper.cpp) und Sprachausgabe
//! (macOS `say` oder Piper), gesteuert über Push-to-Talk.
//!
//! Sicherheitsprinzip: Es gibt **kein Tool**, mit dem das LLM das Mikrofon
//! einschalten könnte. Aufnahmen startet ausschließlich der Mensch
//! (Hotkey/Button). Whisper läuft als kurzlebiger Prozess je Aufnahme und
//! belegt danach keinen Speicher mehr.

use async_trait::async_trait;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum VoiceError {
    #[error("nicht installiert: {0}")]
    NotInstalled(String),
    #[error("Prozessfehler: {0}")]
    Process(String),
    #[error("ungültiger Zustand: {0}")]
    State(String),
    #[error("Zeitüberschreitung")]
    Timeout,
}

#[async_trait]
pub trait SpeechToText: Send + Sync {
    async fn transcribe(&self, wav: &Path) -> Result<String, VoiceError>;
}

#[async_trait]
pub trait TextToSpeech: Send + Sync {
    async fn speak(&self, text: &str) -> Result<(), VoiceError>;
    async fn stop(&self);
}

/// whisper.cpp-CLI (`brew install whisper-cpp` → `whisper-cli`).
#[derive(Debug, Clone)]
pub struct WhisperCli {
    pub binary: PathBuf,
    pub model: PathBuf,
    pub language: String,
    pub threads: usize,
    pub timeout: Duration,
}

impl WhisperCli {
    pub fn new(binary: impl Into<PathBuf>, model: impl Into<PathBuf>) -> Self {
        Self { binary: binary.into(), model: model.into(), language: "de".into(), threads: 4, timeout: Duration::from_secs(120) }
    }

    pub fn args(&self, wav: &Path) -> Vec<String> {
        vec![
            "-m".into(),
            self.model.display().to_string(),
            "-l".into(),
            self.language.clone(),
            "-t".into(),
            self.threads.to_string(),
            "-nt".into(),
            "-np".into(),
            "-f".into(),
            wav.display().to_string(),
        ]
    }
}

pub fn clean_transcript(raw: &str) -> String {
    raw.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !(l.starts_with('[') && l.ends_with(']')) && !(l.starts_with('(') && l.ends_with(')')))
        .collect::<Vec<_>>()
        .join(" ")
}

#[async_trait]
impl SpeechToText for WhisperCli {
    async fn transcribe(&self, wav: &Path) -> Result<String, VoiceError> {
        if !self.model.exists() {
            return Err(VoiceError::NotInstalled(format!("Whisper-Modell {}", self.model.display())));
        }
        let child = Command::new(&self.binary)
            .args(self.args(wav))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| VoiceError::NotInstalled(format!("{}: {e}", self.binary.display())))?;
        let out = tokio::time::timeout(self.timeout, child.wait_with_output()).await.map_err(|_| VoiceError::Timeout)?.map_err(|e| VoiceError::Process(e.to_string()))?;
        if !out.status.success() {
            return Err(VoiceError::Process(format!("whisper beendet mit {}", out.status)));
        }
        Ok(clean_transcript(&String::from_utf8_lossy(&out.stdout)))
    }
}

/// macOS-Sprachausgabe über `say` (Stimme z. B. "Anna").
pub struct MacSay {
    pub voice: String,
    pub rate_wpm: u32,
    cancel: tokio::sync::Notify,
}

impl MacSay {
    pub fn new(voice: impl Into<String>) -> Self {
        Self { voice: voice.into(), rate_wpm: 185, cancel: Default::default() }
    }
}

#[async_trait]
impl TextToSpeech for MacSay {
    async fn speak(&self, text: &str) -> Result<(), VoiceError> {
        self.stop().await;
        // Text über stdin, damit er nie als Argument/Option interpretiert wird.
        let mut child = Command::new("say")
            .args(["-v", &self.voice, "-r", &self.rate_wpm.to_string()])
            .stdin(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| VoiceError::NotInstalled(format!("say: {e}")))?;
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(text.as_bytes()).await.map_err(|e| VoiceError::Process(e.to_string()))?;
        drop(stdin);
        tokio::select! {
            r = child.wait() => { r.map_err(|e| VoiceError::Process(e.to_string()))?; }
            _ = self.cancel.notified() => { let _ = child.kill().await; }
        }
        Ok(())
    }
    async fn stop(&self) {
        self.cancel.notify_waiters();
    }
}

/// Piper (optional): `piper --model de_DE-thorsten-medium.onnx --output_file x.wav`,
/// danach Wiedergabe mit `afplay`.
pub struct Piper {
    pub binary: PathBuf,
    pub model: PathBuf,
    pub player: String,
}

#[async_trait]
impl TextToSpeech for Piper {
    async fn speak(&self, text: &str) -> Result<(), VoiceError> {
        let dir = std::env::temp_dir().join(format!("jarvis-tts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| VoiceError::Process(e.to_string()))?;
        let wav = dir.join("out.wav");
        let mut child = Command::new(&self.binary)
            .arg("--model")
            .arg(&self.model)
            .arg("--output_file")
            .arg(&wav)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| VoiceError::NotInstalled(format!("piper: {e}")))?;
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(text.as_bytes()).await.map_err(|e| VoiceError::Process(e.to_string()))?;
        drop(stdin);
        child.wait().await.map_err(|e| VoiceError::Process(e.to_string()))?;
        let st = Command::new(&self.player).arg(&wav).status().await.map_err(|e| VoiceError::NotInstalled(format!("{}: {e}", self.player)))?;
        let _ = std::fs::remove_file(&wav);
        st.success().then_some(()).ok_or_else(|| VoiceError::Process("Wiedergabe fehlgeschlagen".into()))
    }
    async fn stop(&self) {}
}

/// Zustände der Sprachsteuerung.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum VoiceState {
    Disabled,
    Idle,
    Recording,
    Transcribing,
    Speaking,
}

/// Push-to-Talk-Zustandsmaschine. Aufnahme nur durch Benutzeraktion.
#[derive(Debug)]
pub struct PushToTalk {
    state: VoiceState,
    pub wake_word_enabled: bool,
}

impl Default for PushToTalk {
    fn default() -> Self {
        Self { state: VoiceState::Idle, wake_word_enabled: false }
    }
}

impl PushToTalk {
    pub fn state(&self) -> VoiceState {
        self.state
    }

    fn go(&mut self, from: &[VoiceState], to: VoiceState) -> Result<(), VoiceError> {
        if from.contains(&self.state) {
            self.state = to;
            Ok(())
        } else {
            Err(VoiceError::State(format!("{:?} -> {:?}", self.state, to)))
        }
    }

    /// Hotkey gedrückt (nur aus der UI aufrufbar).
    pub fn press(&mut self) -> Result<(), VoiceError> {
        self.go(&[VoiceState::Idle, VoiceState::Speaking], VoiceState::Recording)
    }
    /// Hotkey losgelassen.
    pub fn release(&mut self) -> Result<(), VoiceError> {
        self.go(&[VoiceState::Recording], VoiceState::Transcribing)
    }
    pub fn transcribed(&mut self) -> Result<(), VoiceError> {
        self.go(&[VoiceState::Transcribing], VoiceState::Idle)
    }
    pub fn speaking(&mut self) -> Result<(), VoiceError> {
        self.go(&[VoiceState::Idle], VoiceState::Speaking)
    }
    pub fn spoken(&mut self) -> Result<(), VoiceError> {
        self.go(&[VoiceState::Speaking], VoiceState::Idle)
    }
    /// Vom Resource Manager (Modus "Critical").
    pub fn set_enabled(&mut self, on: bool) {
        self.state = if on { VoiceState::Idle } else { VoiceState::Disabled };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_to_talk_flow() {
        let mut p = PushToTalk::default();
        assert!(p.release().is_err(), "loslassen ohne drücken");
        p.press().unwrap();
        assert_eq!(p.state(), VoiceState::Recording);
        assert!(p.press().is_err());
        p.release().unwrap();
        p.transcribed().unwrap();
        p.speaking().unwrap();
        p.press().unwrap(); // Unterbrechen während JARVIS spricht
        assert_eq!(p.state(), VoiceState::Recording);
        p.set_enabled(false);
        assert!(p.press().is_err(), "deaktiviert");
        assert!(!p.wake_word_enabled, "Wake Word standardmäßig aus");
    }

    #[test]
    fn transcript_cleanup() {
        assert_eq!(clean_transcript(" Hallo JARVIS.\n[Musik]\n(Husten)\n wie spät ist es?\n"), "Hallo JARVIS. wie spät ist es?");
    }

    #[test]
    fn whisper_args() {
        let w = WhisperCli::new("whisper-cli", "/m/ggml-small.bin");
        let a = w.args(Path::new("/tmp/a.wav"));
        assert!(a.windows(2).any(|x| x == ["-l", "de"]));
        assert!(a.contains(&"-nt".to_string()));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn whisper_with_fake_binary() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let bin = d.path().join("whisper-cli");
        std::fs::write(&bin, "#!/bin/sh\necho ' Zeig mir den Stundenplan.'\necho '[Musik]'\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let model = d.path().join("m.bin");
        std::fs::write(&model, "x").unwrap();
        let w = WhisperCli::new(&bin, &model);
        assert_eq!(w.transcribe(Path::new("x.wav")).await.unwrap(), "Zeig mir den Stundenplan.");
        let missing = WhisperCli::new(&bin, d.path().join("fehlt.bin"));
        assert!(matches!(missing.transcribe(Path::new("x.wav")).await, Err(VoiceError::NotInstalled(_))));
    }
}
