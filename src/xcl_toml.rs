//! Writes the launcher's display and input settings into a title's `xcl.toml`
//! (`<data root>/<title>/xcl.toml`), keeping the rest of the file and its comments as they are.

use std::path::Path;

use anyhow::{Context, Result};
use toml_edit::{DocumentMut, value};

use crate::config::LaunchSettings;

const NEW_FILE_HEADER: &str = "# XCL – title configuration, started by XCL Launcher.\n\
# XCL uses its defaults for every setting not written here; you can edit this file.\n\n";

/// Applies the settings that live in `xcl.toml`. Settings left unset are not touched. The file
/// is only rewritten when something changes. Returns whether it was written.
pub fn apply(path: &Path, settings: &LaunchSettings) -> Result<bool> {
    let entries = entries(settings);
    if entries.is_empty() {
        return Ok(false);
    }
    let original = match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut doc: DocumentMut = original
        .as_deref()
        .unwrap_or(NEW_FILE_HEADER)
        .parse()
        .with_context(|| format!("{} is not valid TOML", path.display()))?;
    for (key, item) in entries {
        doc[key] = item;
    }
    let text = doc.to_string();
    if original.as_deref() == Some(text.as_str()) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(true)
}

fn entries(s: &LaunchSettings) -> Vec<(&'static str, toml_edit::Item)> {
    let mut out = Vec::new();
    if let Some(w) = s.window_width {
        out.push(("window_width", value(i64::from(w))));
    }
    if let Some(h) = s.window_height {
        out.push(("window_height", value(i64::from(h))));
    }
    if let Some(p) = s.window_pos {
        out.push(("window_pos", value(p.xcl_value())));
    }
    if let Some(tag) = s
        .gamertag
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        out.push(("gamertag", value(tag)));
    }
    if let Some(on) = s.keyboard_mouse {
        out.push(("keyboard_mouse", value(on)));
    }
    if let Some(m) = s.mouse_sensitivity {
        // f32 → f64 through its decimal form, so 1.1 is written as 1.1 and not 1.100000023841858
        out.push((
            "mouse_sensitivity",
            value(m.to_string().parse::<f64>().unwrap_or(f64::from(m))),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WindowPos;

    fn temp_file(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("xcl-launcher-toml-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("xcl.toml")
    }

    #[test]
    fn keeps_comments_tables_and_other_keys() {
        let path = temp_file("keep");
        std::fs::write(
            &path,
            "# comment\ngamertag = \"P\"\nwindow_width = 1280\n\n[mounts]\n\"G:\" = \"{game}\"\n",
        )
        .unwrap();
        let s = LaunchSettings {
            window_width: Some(1920),
            window_pos: Some(WindowPos::At { x: 0, y: 0 }),
            mouse_sensitivity: Some(1.1),
            ..Default::default()
        };
        assert!(apply(&path, &s).unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        assert!(text.starts_with("# comment\n"));
        assert!(text.contains("window_width = 1920"));
        assert!(text.contains("window_pos = \"0,0\""));
        assert!(text.contains("mouse_sensitivity = 1.1\n"));
        assert!(text.contains("gamertag = \"P\""));
        // top-level keys must stay above the first table
        assert!(text.find("window_pos").unwrap() < text.find("[mounts]").unwrap());
    }

    #[test]
    fn unchanged_settings_do_not_rewrite_or_create() {
        let path = temp_file("same");
        assert!(!apply(&path, &LaunchSettings::default()).unwrap());
        assert!(!path.exists());
        let s = LaunchSettings {
            window_height: Some(1080),
            ..Default::default()
        };
        assert!(apply(&path, &s).unwrap());
        assert!(!apply(&path, &s).unwrap());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
