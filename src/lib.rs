// Message enums (commands, outcomes) are short-lived; boxing their large variants gains nothing.
#![allow(clippy::large_enum_variant)]

pub mod db;
pub mod engine;
pub mod error;
pub mod export;
pub mod net;
pub mod paths;
pub mod platform;
pub mod player;
pub mod settings;
pub mod startup;
pub mod subs;
pub mod subtitles;
pub mod theme;
pub mod thumbs;
pub mod tools;
pub mod trim;
pub mod ui;
pub mod util;
pub mod ytdlp;
