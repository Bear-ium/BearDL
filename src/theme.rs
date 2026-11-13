//! TOML themes: colours, fonts and spacing. Built-in Light and Dark, plus any `*.toml`
//! in the user themes folder. Every field is optional and falls back to the base theme.
use eframe::egui::{self, Color32, FontFamily, FontId, TextStyle};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const DARK: &str = r##"name = "Dark"
dark = true

[colours]
background = "#1b1c1e"
panel = "#202124"
text = "#dcdde0"
weak_text = "#8b8d93"
accent = "#2f7fd1"
accent_text = "#ffffff"
widget = "#2b2d31"
widget_hover = "#373a40"
border = "#3a3d43"
error = "#ef6b73"
warning = "#e5b55c"
link = "#6aaaf0"

[fonts]
size = 14.0
heading = 20.0
small = 11.5
monospace = 13.0

[spacing]
item_x = 8.0
item_y = 6.0
button_x = 8.0
button_y = 4.0
rounding = 5.0
"##;

pub const LIGHT: &str = r##"name = "Light"
dark = false

[colours]
background = "#f6f6f7"
panel = "#ececee"
text = "#1e1f22"
weak_text = "#6b6e75"
accent = "#1f6fc4"
accent_text = "#ffffff"
widget = "#e2e3e6"
widget_hover = "#d5d7db"
border = "#c9cbd0"
error = "#c4313b"
warning = "#a86b00"
link = "#1f6fc4"

[fonts]
size = 14.0
heading = 20.0
small = 11.5
monospace = 13.0

[spacing]
item_x = 8.0
item_y = 6.0
button_x = 8.0
button_y = 4.0
rounding = 5.0
"##;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Colours {
    pub background: Option<String>,
    pub panel: Option<String>,
    pub text: Option<String>,
    pub weak_text: Option<String>,
    pub accent: Option<String>,
    pub accent_text: Option<String>,
    pub widget: Option<String>,
    pub widget_hover: Option<String>,
    pub border: Option<String>,
    pub error: Option<String>,
    pub warning: Option<String>,
    pub link: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Fonts {
    pub size: Option<f32>,
    pub heading: Option<f32>,
    pub small: Option<f32>,
    pub monospace: Option<f32>,
    /// Path to a .ttf/.otf used for the UI font.
    pub family: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Spacing {
    pub item_x: Option<f32>,
    pub item_y: Option<f32>,
    pub button_x: Option<f32>,
    pub button_y: Option<f32>,
    pub rounding: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Theme {
    pub name: String,
    pub dark: bool,
    pub colours: Colours,
    pub fonts: Fonts,
    pub spacing: Spacing,
}

/// Parses `#rgb`, `#rrggbb` or `#rrggbbaa`.
pub fn parse_colour(s: &str) -> Option<Color32> {
    let h = s.trim().strip_prefix('#')?;
    let v = |i: usize, n: usize| u8::from_str_radix(&h[i..i + n], 16).ok();
    match h.len() {
        3 => {
            let d = |i| v(i, 1).map(|x| x * 17);
            Some(Color32::from_rgb(d(0)?, d(1)?, d(2)?))
        }
        6 => Some(Color32::from_rgb(v(0, 2)?, v(2, 2)?, v(4, 2)?)),
        8 => Some(Color32::from_rgba_unmultiplied(
            v(0, 2)?,
            v(2, 2)?,
            v(4, 2)?,
            v(6, 2)?,
        )),
        _ => None,
    }
}

impl Theme {
    pub fn parse(text: &str) -> Result<Theme, String> {
        let t: Theme = toml::from_str(text).map_err(|e| e.message().to_owned())?;
        if t.name.trim().is_empty() {
            return Err("a theme needs a name".into());
        }
        let c = &t.colours;
        for (k, v) in [
            ("background", &c.background),
            ("panel", &c.panel),
            ("text", &c.text),
            ("weak_text", &c.weak_text),
            ("accent", &c.accent),
            ("accent_text", &c.accent_text),
            ("widget", &c.widget),
            ("widget_hover", &c.widget_hover),
            ("border", &c.border),
            ("error", &c.error),
            ("warning", &c.warning),
            ("link", &c.link),
        ] {
            if let Some(v) = v
                && parse_colour(v).is_none()
            {
                return Err(format!("colours.{k}: “{v}” is not a #rrggbb colour"));
            }
        }
        Ok(t)
    }

    pub fn font_path(&self) -> Option<PathBuf> {
        self.fonts
            .family
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
    }

    /// Applies the theme to an egui style. Unset fields keep egui's light or dark default.
    pub fn apply(&self, style: &mut egui::Style) {
        let mut v = if self.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        let c = |o: &Option<String>| o.as_deref().and_then(parse_colour);
        let col = &self.colours;
        if let Some(bg) = c(&col.background) {
            v.panel_fill = bg;
            v.window_fill = bg;
        }
        if let Some(p) = c(&col.panel) {
            v.faint_bg_color = p;
            v.extreme_bg_color = if self.dark {
                p.gamma_multiply(0.7)
            } else {
                Color32::WHITE
            };
            v.code_bg_color = p;
        }
        if let Some(t) = c(&col.text) {
            v.override_text_color = Some(t);
            v.widgets.noninteractive.fg_stroke.color = t;
            v.widgets.inactive.fg_stroke.color = t;
            v.widgets.hovered.fg_stroke.color = t;
            v.widgets.active.fg_stroke.color = t;
        }
        if let Some(w) = c(&col.weak_text) {
            v.weak_text_color = Some(w);
        }
        if let Some(w) = c(&col.widget) {
            v.widgets.inactive.weak_bg_fill = w;
            v.widgets.inactive.bg_fill = w;
        }
        if let Some(h) = c(&col.widget_hover) {
            v.widgets.hovered.weak_bg_fill = h;
            v.widgets.hovered.bg_fill = h;
            v.widgets.active.weak_bg_fill = h;
            v.widgets.active.bg_fill = h;
        }
        if let Some(b) = c(&col.border) {
            v.widgets.noninteractive.bg_stroke.color = b;
            v.window_stroke.color = b;
        }
        if let Some(a) = c(&col.accent) {
            v.selection.bg_fill = a;
            v.hyperlink_color = c(&col.link).unwrap_or(a);
        } else if let Some(l) = c(&col.link) {
            v.hyperlink_color = l;
        }
        if let Some(at) = c(&col.accent_text) {
            v.selection.stroke.color = at;
        }
        if let Some(e) = c(&col.error) {
            v.error_fg_color = e;
        }
        if let Some(w) = c(&col.warning) {
            v.warn_fg_color = w;
        }
        if let Some(r) = self.spacing.rounding {
            let r = r.clamp(0.0, 20.0);
            for w in [
                &mut v.widgets.noninteractive,
                &mut v.widgets.inactive,
                &mut v.widgets.hovered,
                &mut v.widgets.active,
                &mut v.widgets.open,
            ] {
                w.corner_radius = egui::CornerRadius::same(r as u8);
            }
            v.window_corner_radius = egui::CornerRadius::same((r * 1.5) as u8);
        }
        style.visuals = v;

        let f = &self.fonts;
        let size = f.size.unwrap_or(14.0).clamp(8.0, 40.0);
        style.text_styles = [
            (
                TextStyle::Heading,
                FontId::new(
                    f.heading.unwrap_or(size * 1.4).clamp(8.0, 60.0),
                    FontFamily::Proportional,
                ),
            ),
            (TextStyle::Body, FontId::new(size, FontFamily::Proportional)),
            (
                TextStyle::Button,
                FontId::new(size, FontFamily::Proportional),
            ),
            (
                TextStyle::Small,
                FontId::new(
                    f.small.unwrap_or(size * 0.8).clamp(6.0, 40.0),
                    FontFamily::Proportional,
                ),
            ),
            (
                TextStyle::Monospace,
                FontId::new(
                    f.monospace.unwrap_or(size * 0.95).clamp(6.0, 40.0),
                    FontFamily::Monospace,
                ),
            ),
        ]
        .into();

        let s = &self.spacing;
        if let (Some(x), Some(y)) = (s.item_x, s.item_y) {
            style.spacing.item_spacing = egui::vec2(x.clamp(0.0, 40.0), y.clamp(0.0, 40.0));
        }
        if let (Some(x), Some(y)) = (s.button_x, s.button_y) {
            style.spacing.button_padding = egui::vec2(x.clamp(0.0, 40.0), y.clamp(0.0, 40.0));
        }
    }
}

pub fn builtins() -> Vec<Theme> {
    [DARK, LIGHT]
        .iter()
        .filter_map(|t| Theme::parse(t).ok())
        .collect()
}

/// A theme file in the user folder, or a parse error for it.
#[derive(Clone, Debug)]
pub struct UserTheme {
    pub path: PathBuf,
    pub theme: Result<Theme, String>,
}

pub fn load_user_themes(dir: &Path) -> Vec<UserTheme> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<UserTheme> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("toml"))
        })
        .map(|path| {
            let theme = std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|s| Theme::parse(&s));
            UserTheme { path, theme }
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Writes an example theme the first time the folder is empty, so people have a template.
pub fn write_example(dir: &Path) {
    let has_any = std::fs::read_dir(dir)
        .map(|mut r| r.next().is_some())
        .unwrap_or(false);
    if has_any {
        return;
    }
    let _ = std::fs::create_dir_all(dir);
    let example = DARK
        .replacen("name = \"Dark\"", "name = \"Midnight\"", 1)
        .replace("#2f7fd1", "#8a63d2")
        .replace("#6aaaf0", "#b39af0");
    let text = format!(
        "# Example theme. Copy this file, change the name and colours, then pick it in\n\
         # Settings > Appearance. Every field is optional.\n\
         # fonts.family can point to a .ttf or .otf file.\n{example}"
    );
    let _ = std::fs::write(dir.join("midnight.toml"), text);
}

/// Finds a theme by name: user themes override built-ins of the same name.
pub fn find(name: &str, user: &[UserTheme]) -> Theme {
    user.iter()
        .filter_map(|u| u.theme.as_ref().ok())
        .find(|t| t.name == name)
        .cloned()
        .or_else(|| builtins().into_iter().find(|t| t.name == name))
        .unwrap_or_else(|| builtins().remove(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours() {
        assert_eq!(parse_colour("#fff"), Some(Color32::WHITE));
        assert_eq!(parse_colour("#102030"), Some(Color32::from_rgb(16, 32, 48)));
        assert!(parse_colour("#10203080").is_some());
        assert!(parse_colour("102030").is_none());
        assert!(parse_colour("#zzzzzz").is_none());
    }

    #[test]
    fn builtins_parse_and_apply() {
        let b = builtins();
        assert_eq!(b.len(), 2);
        let mut style = egui::Style::default();
        b[1].apply(&mut style);
        assert!(!style.visuals.dark_mode);
        assert_eq!(
            style.visuals.error_fg_color,
            parse_colour("#c4313b").unwrap()
        );
        b[0].apply(&mut style);
        assert!(style.visuals.dark_mode);
        assert_eq!(style.text_styles[&TextStyle::Body].size, 14.0);
    }

    #[test]
    fn partial_and_invalid_themes() {
        let t = Theme::parse("name = \"Mini\"\n[colours]\naccent = \"#ff0000\"\n").unwrap();
        let mut style = egui::Style::default();
        t.apply(&mut style);
        assert_eq!(
            style.visuals.selection.bg_fill,
            Color32::from_rgb(255, 0, 0)
        );
        assert!(Theme::parse("name = \"Bad\"\n[colours]\ntext = \"red\"\n").is_err());
        assert!(Theme::parse("[colours]\n").is_err());
        assert!(Theme::parse("name = 3").is_err());
    }

    #[test]
    fn user_themes_override_and_example() {
        let d = std::env::temp_dir().join(format!("beardl-themes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        write_example(&d);
        std::fs::write(d.join("dark.toml"), "name = \"Dark\"\ndark = false\n").unwrap();
        std::fs::write(d.join("broken.toml"), "name = ").unwrap();
        let u = load_user_themes(&d);
        assert_eq!(u.len(), 3);
        assert!(u.iter().any(|t| t.theme.is_err()));
        assert!(!find("Dark", &u).dark, "user theme overrides built-in");
        assert_eq!(find("Midnight", &u).name, "Midnight");
        assert_eq!(find("Nope", &u).name, "Dark");
        std::fs::remove_dir_all(d).unwrap();
    }
}
