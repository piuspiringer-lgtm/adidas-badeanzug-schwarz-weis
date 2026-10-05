//! Verifikation: prüft nach verändernden Aktionen deterministisch, ob das
//! gewünschte Ergebnis wirklich eingetreten ist – unabhängig davon, was das
//! Modell behauptet.

use serde_json::Value;
use std::path::Path;

/// `None`: für dieses Tool gibt es keine Nachbedingung (z. B. Lese-Tools).
pub fn verify_effect(tool: &str, data: &Value) -> Option<Result<String, String>> {
    let p = |k: &str| data.get(k).and_then(Value::as_str).map(Path::new);
    match tool {
        "fs_create" => {
            let path = p("path")?;
            Some(if path.is_file() { Ok(format!("{} existiert", path.display())) } else { Err(format!("{} fehlt nach dem Erstellen", path.display())) })
        }
        "fs_move" | "fs_rename" => {
            let (from, to) = (p("from")?, p("to")?);
            Some(match (from.exists(), to.exists()) {
                (false, true) => Ok(format!("{} liegt jetzt unter {}", from.display(), to.display())),
                (_, false) => Err(format!("Ziel {} existiert nicht", to.display())),
                (true, true) => Err(format!("Quelle {} existiert noch", from.display())),
            })
        }
        "fs_copy" => {
            let (from, to) = (p("from")?, p("to")?);
            Some(if from.exists() && to.exists() { Ok(format!("Kopie {} vorhanden", to.display())) } else { Err("Kopie oder Original fehlt".into()) })
        }
        "fs_trash" => {
            let path = p("path")?;
            Some(if !path.exists() { Ok(format!("{} im Papierkorb", path.display())) } else { Err(format!("{} existiert noch", path.display())) })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn postconditions() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a.txt");
        let b = d.path().join("b.txt");
        std::fs::write(&a, "x").unwrap();
        assert!(verify_effect("fs_create", &json!({"path": a})).unwrap().is_ok());
        assert!(verify_effect("fs_move", &json!({"from": a, "to": b})).unwrap().is_err());
        std::fs::rename(&a, &b).unwrap();
        assert!(verify_effect("fs_move", &json!({"from": a, "to": b})).unwrap().is_ok());
        assert!(verify_effect("fs_trash", &json!({"path": b})).unwrap().is_err());
        assert!(verify_effect("fs_read", &json!({})).is_none());
    }
}
