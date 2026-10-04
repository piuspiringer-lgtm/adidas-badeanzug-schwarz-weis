//! Modell-Routing: einfache Aufgaben → kleines lokales Modell, normale →
//! lokales Hauptmodell, komplexe → Hauptmodell bzw. optional ein stärkeres
//! Modell. Ein externes Modell ist standardmäßig aus und wird nur genutzt,
//! wenn der Benutzer es ausdrücklich aktiviert und konfiguriert hat.

use crate::estimate_tokens;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Complexity {
    Simple,
    Standard,
    Complex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ModelTier {
    /// Kleines lokales Modell (Fallback, z. B. 4B).
    LocalSmall,
    /// Lokales Hauptmodell (z. B. 8B).
    LocalMain,
    /// Optionales stärkeres Modell (lokal oder extern) – nur nach Opt-in.
    Strong,
}

#[derive(Debug, Clone, Serialize)]
pub struct Router {
    /// Stärkeres Modell konfiguriert und vom Benutzer freigegeben.
    pub strong_enabled: bool,
    /// Ressourcenmodus erzwingt das kleine Modell (Akku/Thermik/Speicher).
    pub force_small: bool,
}

const COMPLEX_HINTS: &[&str] = &[
    "vergleiche", "analysiere", "plane", "schritt für schritt", "begründe", "recherchiere", "erkläre ausführlich",
    "programmiere", "code", "zusammenhang", "strategie", "compare", "analyze", "plan",
];
const SIMPLE_HINTS: &[&str] = &[
    "hallo", "hi", "danke", "wie spät", "uhrzeit", "datum", "öffne", "zeige", "stopp", "lauter", "leiser", "ja", "nein",
];

impl Router {
    pub fn classify(&self, prompt: &str, tool_count: usize) -> Complexity {
        let p = prompt.to_lowercase();
        let tokens = estimate_tokens(prompt);
        let complex_hits = COMPLEX_HINTS.iter().filter(|h| p.contains(*h)).count();
        if tokens > 600 || complex_hits >= 2 || (complex_hits == 1 && tool_count > 2) {
            Complexity::Complex
        } else if tokens < 25 && complex_hits == 0 && tool_count <= 1 && SIMPLE_HINTS.iter().any(|h| p.contains(h)) {
            Complexity::Simple
        } else {
            Complexity::Standard
        }
    }

    pub fn tier(&self, c: Complexity) -> ModelTier {
        if self.force_small {
            return ModelTier::LocalSmall;
        }
        match c {
            Complexity::Simple => ModelTier::LocalSmall,
            Complexity::Standard => ModelTier::LocalMain,
            Complexity::Complex if self.strong_enabled => ModelTier::Strong,
            Complexity::Complex => ModelTier::LocalMain,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing() {
        let r = Router { strong_enabled: false, force_small: false };
        assert_eq!(r.classify("Hallo JARVIS", 0), Complexity::Simple);
        assert_eq!(r.classify("Was steht morgen im Stundenplan?", 1), Complexity::Standard);
        assert_eq!(r.classify("Recherchiere und vergleiche drei Laptops", 3), Complexity::Complex);
        assert_eq!(r.tier(Complexity::Simple), ModelTier::LocalSmall);
        assert_eq!(r.tier(Complexity::Complex), ModelTier::LocalMain, "ohne Opt-in nie Strong");
        let r2 = Router { strong_enabled: true, force_small: false };
        assert_eq!(r2.tier(Complexity::Complex), ModelTier::Strong);
        let r3 = Router { strong_enabled: true, force_small: true };
        assert_eq!(r3.tier(Complexity::Complex), ModelTier::LocalSmall);
    }
}
