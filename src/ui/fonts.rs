use eframe::egui::{FontData, FontDefinitions, FontFamily};
use std::sync::Arc;

#[cfg(windows)]
const PROPORTIONAL: &[&str] = &[
    r"C:\Windows\Fonts\segoeui.ttf",
    r"C:\Windows\Fonts\arial.ttf",
];
#[cfg(windows)]
const MONO: &[&str] = &[
    r"C:\Windows\Fonts\consola.ttf",
    r"C:\Windows\Fonts\cour.ttf",
];
#[cfg(windows)]
const FALLBACK: &[&str] = &[r"C:\Windows\Fonts\seguisym.ttf"];

#[cfg(target_os = "macos")]
const PROPORTIONAL: &[&str] = &[
    "/System/Library/Fonts/SFNS.ttf",
    "/System/Library/Fonts/Helvetica.ttc",
    "/Library/Fonts/Arial.ttf",
];
#[cfg(target_os = "macos")]
const MONO: &[&str] = &[
    "/System/Library/Fonts/Menlo.ttc",
    "/System/Library/Fonts/Monaco.ttf",
];
#[cfg(target_os = "macos")]
const FALLBACK: &[&str] = &["/System/Library/Fonts/Apple Symbols.ttf"];

#[cfg(all(unix, not(target_os = "macos")))]
const PROPORTIONAL: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/google-noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/liberation-sans/LiberationSans-Regular.ttf",
];
#[cfg(all(unix, not(target_os = "macos")))]
const MONO: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
    "/usr/share/fonts/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
];
#[cfg(all(unix, not(target_os = "macos")))]
const FALLBACK: &[&str] = &[
    "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
    "/usr/share/fonts/noto/NotoSansSymbols2-Regular.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
];

const MAX_FONT_BYTES: u64 = 50 * 1024 * 1024;

/// Reads a user font only if it is a local file of sane size that parses as a font, so a
/// bad theme can never crash the font system (epaint panics on unparseable fonts).
pub fn load_custom_font(path: &std::path::Path) -> Option<Vec<u8>> {
    let s = path.to_string_lossy();
    if s.starts_with("\\\\") || s.starts_with("//") {
        return None;
    }
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_FONT_BYTES {
        return None;
    }
    let data = std::fs::read(path).ok()?;
    skrifa::FontRef::from_index(&data, 0).ok()?;
    Some(data)
}

fn load_first(paths: &[&str]) -> Option<Vec<u8>> {
    paths.iter().find_map(|p| std::fs::read(p).ok())
}

/// Builds font definitions from fonts already installed on the system, so the
/// binary does not carry its own. `custom` (from a theme) takes priority.
pub fn system_fonts(custom: Option<&std::path::Path>) -> FontDefinitions {
    let mut defs = FontDefinitions::empty();
    let mut add = |name: &str, data: Vec<u8>| {
        defs.font_data
            .insert(name.to_owned(), Arc::new(FontData::from_owned(data)));
        name.to_owned()
    };
    let mut prop = Vec::new();
    let mut mono = Vec::new();
    if let Some(data) = custom.and_then(load_custom_font) {
        prop.push(add("custom", data));
    }
    if let Some(data) = load_first(PROPORTIONAL) {
        prop.push(add("ui", data));
    }
    if let Some(data) = load_first(MONO) {
        mono.push(add("mono", data));
    }
    let fallback: Vec<String> = FALLBACK
        .iter()
        .enumerate()
        .filter_map(|(i, p)| {
            std::fs::read(p)
                .ok()
                .map(|d| add(&format!("fallback{i}"), d))
        })
        .collect();
    if prop.is_empty() && mono.is_empty() {
        return FontDefinitions::default();
    }
    if mono.is_empty() {
        mono = prop.clone();
    }
    if prop.is_empty() {
        prop = mono.clone();
    }
    prop.extend(fallback);
    for name in &prop {
        if !mono.contains(name) {
            mono.push(name.clone());
        }
    }
    defs.families.insert(FontFamily::Proportional, prop);
    defs.families.insert(FontFamily::Monospace, mono);
    defs
}
