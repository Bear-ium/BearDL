//! yt-dlp integration: option sets (presets), argument building, and parsing of
//! `--progress-template`, `--print` and `--dump-single-json` output.
use crate::util::split_args;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    VideoAudio,
    VideoOnly,
    AudioOnly,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::VideoAudio, Mode::VideoOnly, Mode::AudioOnly];
    pub fn label(self) -> &'static str {
        match self {
            Mode::VideoAudio => "Video + audio",
            Mode::VideoOnly => "Video only",
            Mode::AudioOnly => "Audio only",
        }
    }
}

pub const CONTAINERS: [&str; 5] = ["", "mp4", "mkv", "webm", "mov"];
pub const AUDIO_FORMATS: [&str; 6] = ["", "mp3", "m4a", "opus", "flac", "wav"];

/// A named yt-dlp option set. Stored as JSON in the presets table and in exports.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DownloadOptions {
    pub mode: Mode,
    /// Maximum video height; 0 means no cap.
    pub max_height: u32,
    /// Explicit `-f` selector; overrides mode and height when set.
    pub format: String,
    /// Merge/remux container; empty keeps what yt-dlp picks.
    pub container: String,
    /// Audio conversion target for audio-only downloads; empty keeps the original.
    pub audio_format: String,
    pub audio_quality: String,
    pub embed_metadata: bool,
    pub embed_thumbnail: bool,
    pub embed_chapters: bool,
    pub subtitles: bool,
    pub auto_subtitles: bool,
    pub embed_subs: bool,
    /// Empty uses the global setting.
    pub sub_langs: String,
    /// Sub-folder of the download folder, or an absolute folder.
    pub folder: String,
    /// Empty uses the global template.
    pub output_template: String,
    /// Only download videos uploaded on or after this date (`YYYYMMDD`); empty for no limit.
    pub date_after: String,
    pub custom_args: String,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            mode: Mode::VideoAudio,
            max_height: 0,
            format: String::new(),
            container: String::new(),
            audio_format: String::new(),
            audio_quality: "0".into(),
            embed_metadata: true,
            embed_thumbnail: true,
            embed_chapters: true,
            subtitles: true,
            auto_subtitles: true,
            embed_subs: false,
            sub_langs: String::new(),
            folder: String::new(),
            output_template: String::new(),
            date_after: String::new(),
            custom_args: String::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub options: DownloadOptions,
}

pub fn builtin_presets() -> Vec<Preset> {
    let p = |name: &str, f: &dyn Fn(&mut DownloadOptions)| {
        let mut o = DownloadOptions::default();
        f(&mut o);
        Preset {
            name: name.into(),
            options: o,
        }
    };
    vec![
        p("Best video + audio", &|_| {}),
        p("1080p MP4", &|o| {
            o.max_height = 1080;
            o.container = "mp4".into();
        }),
        p("720p", &|o| o.max_height = 720),
        p("Audio MP3", &|o| {
            o.mode = Mode::AudioOnly;
            o.audio_format = "mp3".into();
        }),
        p("Audio M4A", &|o| {
            o.mode = Mode::AudioOnly;
            o.audio_format = "m4a".into();
        }),
        p("Audio Opus", &|o| {
            o.mode = Mode::AudioOnly;
            o.audio_format = "opus".into();
        }),
        p("Audio FLAC", &|o| {
            o.mode = Mode::AudioOnly;
            o.audio_format = "flac".into();
        }),
    ]
}

impl DownloadOptions {
    /// The `-f` selector. Default is best video plus best audio, merged.
    pub fn format_spec(&self) -> String {
        if !self.format.trim().is_empty() {
            return self.format.trim().to_owned();
        }
        let h = if self.max_height > 0 {
            format!("[height<={}]", self.max_height)
        } else {
            String::new()
        };
        match self.mode {
            Mode::VideoAudio => format!("bv*{h}+ba/b{h}/bv*+ba/b"),
            Mode::VideoOnly => format!("bv{h}/bv*{h}/bv"),
            Mode::AudioOnly => "ba/b".into(),
        }
    }

    /// Thumbnails cannot be embedded in WAV or WebM, and the original audio stream is often WebM.
    pub fn supports_thumbnail_embed(&self) -> bool {
        match self.mode {
            Mode::AudioOnly => !matches!(self.audio_format.as_str(), "" | "wav"),
            _ => self.container != "webm",
        }
    }

    /// Number of streams the selector downloads (for overall progress).
    pub fn expected_parts(&self, format_id: &str) -> usize {
        if !format_id.is_empty() {
            return format_id.split('+').count().max(1);
        }
        match self.mode {
            Mode::VideoAudio => 2,
            _ => 1,
        }
    }
}

/// Everything needed to build a yt-dlp download command line.
pub struct Job<'a> {
    pub url: &'a str,
    pub options: &'a DownloadOptions,
    pub download_dir: &'a Path,
    pub output_template: &'a str,
    pub sub_langs: &'a str,
    pub rate_limit: Option<u64>,
    pub ffmpeg_dir: Option<&'a Path>,
    pub thumbs_dir: &'a Path,
    pub subs_dir: &'a Path,
    /// `--dateafter YYYYMMDD` for subscriptions.
    pub date_after: Option<&'a str>,
    /// Without ffmpeg, merging, conversion and embedding are unavailable.
    pub has_ffmpeg: bool,
}

/// English subtitles, plus the original-language automatic captions (`<lang>-orig`), so
/// non-English videos are indexed in what was actually said rather than a translation.
pub const DEFAULT_SUB_LANGS: &str = "en,en-US,en-GB,.*-orig";

pub const PROGRESS_PREFIX: &str = "[bdl]";
pub const POSTPROCESS_PREFIX: &str = "[bdlpp]";
pub const META_PREFIX: &str = "[bdlmeta]";
pub const FILE_PREFIX: &str = "[bdlfile]";
pub const SUBS_PREFIX: &str = "[bdlsubs]";
pub const THUMB_PREFIX: &str = "[bdlthumb]";

const META_FIELDS: &str = "id,title,channel,uploader,channel_id,uploader_id,duration,upload_date,\
description,thumbnail,webpage_url,original_url,extractor_key,chapters,format_id,resolution,ext,filename";

/// Options managed by the app; custom arguments may not override them.
const RESERVED: &[&str] = &[
    "--print",
    "-O",
    "--print-to-file",
    "-j",
    "--dump-json",
    "-J",
    "--dump-single-json",
    "--print-json",
    "-s",
    "--simulate",
    "--skip-download",
    "-q",
    "--quiet",
    "--progress-template",
    "--no-progress",
    "--progress",
    "--newline",
    "-U",
    "--update",
    "--update-to",
    "--version",
    "-h",
    "--help",
    "-a",
    "--batch-file",
    "--flat-playlist",
    "--load-info-json",
    "-F",
    "--list-formats",
    "--list-subs",
    "--list-thumbnails",
    "--list-extractors",
    "--extractor-descriptions",
    "--no-simulate",
    "--ignore-config",
    "--config-locations",
    "-i",
    "--ignore-errors",
    "--no-ignore-errors",
    "--abort-on-error",
];

/// Short options that take a value; letters after them in a cluster are the value.
const SHORT_WITH_VALUE: &str = "afoNPrRS";
/// Options whose value may legitimately be a URL.
const URL_VALUE_OPTIONS: &[&str] = &[
    "--proxy",
    "--geo-verification-proxy",
    "--referer",
    "--add-header",
    "--add-headers",
    "--impersonate",
    "--xff",
];

/// Checks custom arguments locally: quoting, that they are options, and that none of
/// them clash with what the app manages. yt-dlp accepts unique prefixes of long options
/// and clusters of short flags, so both are expanded before the check. yt-dlp itself
/// checks the rest.
pub fn validate_custom_args(s: &str) -> Result<Vec<String>, String> {
    let args = split_args(s).ok_or("unterminated quote")?;
    if let Some(first) = args.first()
        && !first.starts_with('-')
    {
        return Err(format!("“{first}” is not an option (options start with -)"));
    }
    let reserved_err = |name: &str| format!("{name} is managed by BearDL and cannot be set here");
    let mut prev: Option<&str> = None;
    for a in &args {
        if a == "--" {
            return Err("“--” is not allowed: URLs are added by BearDL".into());
        }
        if let Some(long) = a.strip_prefix("--") {
            let name = format!("--{}", long.split('=').next().unwrap_or(long));
            if name.len() > 3
                && let Some(r) = RESERVED
                    .iter()
                    .find(|r| r.starts_with("--") && r.starts_with(name.as_str()))
            {
                return Err(reserved_err(r));
            }
        } else if let Some(short) = a.strip_prefix('-').filter(|s| !s.is_empty()) {
            for c in short.chars() {
                let flag = format!("-{c}");
                if RESERVED.contains(&flag.as_str()) {
                    return Err(reserved_err(&flag));
                }
                if SHORT_WITH_VALUE.contains(c) {
                    break;
                }
            }
        } else if a.contains("://") && !prev.is_some_and(|p| URL_VALUE_OPTIONS.contains(&p)) {
            return Err(format!(
                "“{a}” looks like a URL; add URLs on the Downloads page"
            ));
        }
        prev = Some(a.as_str());
    }
    Ok(args)
}

/// Options an imported preset may use. Imports are untrusted, so this is an allowlist:
/// anything else (including `--alias`, which can expand to other options, and anything that
/// runs programs, loads code or writes outside the download folder) is refused.
const IMPORT_ALLOWED: &[&str] = &[
    "-N",
    "--concurrent-fragments",
    "-r",
    "--limit-rate",
    "--throttled-rate",
    "-R",
    "--retries",
    "--fragment-retries",
    "--file-access-retries",
    "--retry-sleep",
    "--sleep-requests",
    "--sleep-interval",
    "--min-sleep-interval",
    "--max-sleep-interval",
    "--sleep-subtitles",
    "--http-chunk-size",
    "--force-ipv4",
    "--force-ipv6",
    "--geo-bypass",
    "--xff",
    "--impersonate",
    "--extractor-args",
    "--extractor-retries",
    "-S",
    "--format-sort",
    "--format-sort-force",
    "--prefer-free-formats",
    "--check-formats",
    "--merge-output-format",
    "--remux-video",
    "--recode-video",
    "--audio-quality",
    "--convert-subs",
    "--sub-format",
    "--embed-subs",
    "--embed-thumbnail",
    "--embed-metadata",
    "--embed-chapters",
    "--embed-info-json",
    "--no-embed-subs",
    "--no-embed-thumbnail",
    "--no-embed-metadata",
    "--no-embed-chapters",
    "--split-chapters",
    "--remove-chapters",
    "--sponsorblock-mark",
    "--sponsorblock-remove",
    "--sponsorblock-chapter-title",
    "--no-sponsorblock",
    "--match-filters",
    "--break-match-filters",
    "--download-sections",
    "--live-from-start",
    "--no-live-from-start",
    "--wait-for-video",
    "--playlist-items",
    "--playlist-reverse",
    "--max-filesize",
    "--min-filesize",
    "--no-part",
    "--no-mtime",
    "--write-description",
    "--write-info-json",
    "--xattrs",
    "--trim-filenames",
    "--restrict-filenames",
    "--windows-filenames",
    "--force-keyframes-at-cuts",
];

/// Checks custom arguments that came from an imported file. Every option must be spelled in
/// full and be on the allowlist; values that follow are not interpreted as options.
pub fn check_imported_args(s: &str) -> Result<(), String> {
    validate_custom_args(s)?;
    let args = split_args(s).ok_or("unterminated quote")?;
    for a in &args {
        if !a.starts_with('-') || a.len() < 2 {
            continue;
        }
        let name = a.split('=').next().unwrap_or(a);
        // Short options with an attached value (-N4) are reduced to the option itself.
        let name = if !name.starts_with("--") && name.len() > 2 {
            &name[..2]
        } else {
            name
        };
        if !IMPORT_ALLOWED.contains(&name) {
            return Err(format!("{name} is not allowed in imported presets"));
        }
    }
    Ok(())
}

/// URL that yt-dlp rejects without touching the network.
pub const VALIDATION_URL: &str = "beardl:validate";

/// Arguments that make yt-dlp parse and check `custom`, then stop at an unusable URL.
pub fn validation_args(custom: &[String]) -> Vec<String> {
    let mut v = vec!["--ignore-config".to_owned(), "--simulate".to_owned()];
    v.extend(custom.iter().cloned());
    v.push("--".into());
    v.push(VALIDATION_URL.into());
    v
}

/// Interprets a validation run: option errors are usage errors (exit code 2), while
/// anything that reaches URL handling means the options were accepted.
pub fn validation_result(code: Option<i32>, stderr: &str) -> Result<(), String> {
    let usage = stderr
        .lines()
        .find(|l| l.contains(": error:"))
        .map(|l| l.split(": error:").nth(1).unwrap_or(l).trim().to_owned());
    match (code, usage) {
        (_, Some(msg)) => Err(msg),
        (Some(2), None) => Err(stderr
            .trim()
            .lines()
            .last()
            .unwrap_or("invalid options")
            .into()),
        _ => Ok(()),
    }
}

pub fn common_args() -> Vec<String> {
    ["--ignore-config", "--no-warnings", "--color", "no_color"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Environment for every yt-dlp child: UTF-8 output regardless of the system code page.
pub const ENV: [(&str, &str); 2] = [("PYTHONIOENCODING", "utf-8"), ("PYTHONUTF8", "1")];

pub fn analyse_args(url: &str) -> Vec<String> {
    let mut v = common_args();
    v.extend(["-J", "--flat-playlist", "--", url].map(String::from));
    v
}

/// `--flat-playlist` listing for subscription checks: one JSON line per entry.
pub fn list_args(url: &str, max_items: u32) -> Vec<String> {
    let mut v = common_args();
    v.extend(["--flat-playlist", "-j"].map(String::from));
    if max_items > 0 {
        v.push("--playlist-end".into());
        v.push(max_items.to_string());
    }
    v.push("--".into());
    v.push(url.into());
    v
}

pub fn download_args(job: &Job) -> Vec<String> {
    let o = job.options;
    let mut v = common_args();
    let mut push = |a: &str| v.push(a.to_owned());
    push("--newline");
    push("--progress");
    push("--no-simulate");
    push("--no-playlist");
    push("--no-mtime");
    // A failed subtitle or thumbnail must not lose the video; success is judged by the final file.
    push("--ignore-errors");
    push("--progress-template");
    push(&format!(
        "download:{PROGRESS_PREFIX}%(progress.status)s|%(progress.downloaded_bytes)s|\
%(progress.total_bytes)s|%(progress.total_bytes_estimate)s|%(progress.speed)s|\
%(progress.eta)s|%(info.format_id)s"
    ));
    push("--progress-template");
    push(&format!(
        "postprocess:{POSTPROCESS_PREFIX}%(progress.status)s|%(progress.postprocessor)s"
    ));
    push("--print");
    push(&format!("video:{META_PREFIX}%(.{{{META_FIELDS}}})j"));
    push("--print");
    push(&format!("after_move:{FILE_PREFIX}%(filepath)s"));
    push("--print");
    push(&format!("after_move:{SUBS_PREFIX}%(requested_subtitles)j"));
    push("--print");
    push(&format!(
        "after_move:{THUMB_PREFIX}%(thumbnails.-1.filepath)s"
    ));

    let folder = o.folder.trim();
    let dir = if folder.is_empty() {
        job.download_dir.to_path_buf()
    } else if Path::new(folder).is_absolute() {
        folder.into()
    } else {
        job.download_dir.join(folder)
    };
    push("-P");
    push(&dir.to_string_lossy());
    push("-o");
    let tpl = if o.output_template.trim().is_empty() {
        job.output_template
    } else {
        o.output_template.trim()
    };
    push(tpl);
    // Library thumbnails and subtitles for indexing go to the app cache.
    push("--write-thumbnail");
    if job.has_ffmpeg {
        push("--convert-thumbnails");
        push("jpg");
    }
    push("-o");
    push(&format!(
        "thumbnail:{}",
        job.thumbs_dir
            .join("%(extractor_key)s-%(id)s.%(ext)s")
            .to_string_lossy()
    ));
    if o.subtitles || o.auto_subtitles {
        if o.subtitles {
            push("--write-subs");
        }
        if o.auto_subtitles {
            push("--write-auto-subs");
        }
        push("--sub-langs");
        let langs = if o.sub_langs.trim().is_empty() {
            job.sub_langs
        } else {
            o.sub_langs.trim()
        };
        push(if langs.is_empty() {
            DEFAULT_SUB_LANGS
        } else {
            langs
        });
        push("--sub-format");
        push("vtt/srt/best");
        if job.has_ffmpeg {
            // Whatever format was picked, the index reads VTT.
            push("--convert-subs");
            push("vtt");
        }
        push("-o");
        push(&format!(
            "subtitle:{}",
            job.subs_dir
                .join("%(extractor_key)s-%(id)s.%(ext)s")
                .to_string_lossy()
        ));
        if o.embed_subs && job.has_ffmpeg {
            push("--embed-subs");
        }
    }

    push("-f");
    if job.has_ffmpeg || !o.format.trim().is_empty() || o.mode != Mode::VideoAudio {
        push(&o.format_spec());
    } else {
        // Separate streams cannot be merged without ffmpeg: take the best single file.
        let h = if o.max_height > 0 {
            format!("b[height<={}]/b", o.max_height)
        } else {
            "b".into()
        };
        push(&h);
    }
    if !job.has_ffmpeg {
        // Nothing below works without ffmpeg.
    } else if o.mode == Mode::AudioOnly {
        if !o.audio_format.is_empty() {
            push("-x");
            push("--audio-format");
            push(&o.audio_format);
            push("--audio-quality");
            push(if o.audio_quality.is_empty() {
                "0"
            } else {
                &o.audio_quality
            });
        }
    } else if !o.container.is_empty() {
        push("--merge-output-format");
        push(&o.container);
        push("--remux-video");
        push(&o.container);
    }
    if job.has_ffmpeg {
        if o.embed_metadata {
            push("--embed-metadata");
        }
        if o.embed_thumbnail && o.supports_thumbnail_embed() {
            push("--embed-thumbnail");
        }
        if o.embed_chapters {
            push("--embed-chapters");
        }
    }
    if let Some(r) = job.rate_limit {
        push("--limit-rate");
        push(&r.to_string());
    }
    if let Some(d) = job.date_after {
        push("--dateafter");
        push(d);
    }
    if let Some(f) = job.ffmpeg_dir {
        push("--ffmpeg-location");
        push(&f.to_string_lossy());
    }
    if let Ok(extra) = validate_custom_args(&o.custom_args) {
        v.extend(extra);
    }
    v.push("--".into());
    v.push(job.url.into());
    v
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Progress {
    pub status: String,
    pub downloaded: Option<u64>,
    pub total: Option<u64>,
    pub speed: Option<f64>,
    pub eta: Option<f64>,
    pub format_id: String,
}

fn num(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

#[derive(Clone, Debug, PartialEq)]
pub enum Line {
    Progress(Progress),
    PostProcess { status: String, name: String },
    Meta(Box<Value>),
    File(String),
    Thumb(String),
    Subs(Box<Value>),
    Error(String),
    Other,
}

/// Classifies one output line from a download run.
pub fn parse_line(line: &str) -> Line {
    let line = line.trim_end_matches(['\r', '\n']);
    if let Some(rest) = line.strip_prefix(PROGRESS_PREFIX) {
        let f: Vec<&str> = rest.split('|').collect();
        if f.len() < 7 {
            return Line::Other;
        }
        let total = num(f[2]).or_else(|| num(f[3]));
        return Line::Progress(Progress {
            status: f[0].to_owned(),
            downloaded: num(f[1]).map(|v| v as u64),
            total: total.map(|v| v as u64),
            speed: num(f[4]),
            eta: num(f[5]),
            format_id: f[6].trim().to_owned(),
        });
    }
    if let Some(rest) = line.strip_prefix(POSTPROCESS_PREFIX) {
        let (status, name) = rest.split_once('|').unwrap_or((rest, ""));
        return Line::PostProcess {
            status: status.into(),
            name: name.into(),
        };
    }
    if let Some(rest) = line.strip_prefix(META_PREFIX) {
        return serde_json::from_str(rest)
            .map(|v| Line::Meta(Box::new(v)))
            .unwrap_or(Line::Other);
    }
    if let Some(rest) = line.strip_prefix(FILE_PREFIX) {
        return Line::File(rest.trim().to_owned());
    }
    if let Some(rest) = line.strip_prefix(THUMB_PREFIX) {
        let t = rest.trim();
        return if t.is_empty() || t == "NA" {
            Line::Other
        } else {
            Line::Thumb(t.to_owned())
        };
    }
    if let Some(rest) = line.strip_prefix(SUBS_PREFIX) {
        return serde_json::from_str(rest)
            .map(|v| Line::Subs(Box::new(v)))
            .unwrap_or(Line::Other);
    }
    if let Some(rest) = line.strip_prefix("ERROR:") {
        return Line::Error(rest.trim().to_owned());
    }
    Line::Other
}

/// Friendly name for a post-processor stage.
pub fn postprocessor_label(name: &str) -> &'static str {
    match name {
        "Merger" | "FFmpegMerger" => "Merging",
        "ExtractAudio" | "FFmpegExtractAudio" => "Converting audio",
        "VideoRemuxer" | "FFmpegVideoRemuxer" => "Remuxing",
        "VideoConvertor" | "FFmpegVideoConvertor" => "Converting",
        "EmbedThumbnail" => "Embedding thumbnail",
        "Metadata" | "FFmpegMetadata" => "Writing metadata",
        "EmbedSubtitle" | "FFmpegEmbedSubtitle" => "Embedding subtitles",
        "ThumbnailsConvertor" | "FFmpegThumbnailsConvertor" => "Converting thumbnail",
        "SubtitlesConvertor" | "FFmpegSubtitlesConvertor" => "Converting subtitles",
        "MoveFiles" | "MoveFilesAfterDownload" => "Moving files",
        _ => "Processing",
    }
}

/// True when yt-dlp skipped the video because of a filter (date cutoff, match filter).
pub fn is_filter_skip(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("not in range")
        || t.contains("does not pass filter")
        || t.contains("skipping due to")
        || t.contains("has already been recorded in the archive")
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Meta {
    pub id: String,
    pub title: String,
    pub channel: String,
    pub channel_id: String,
    pub duration: Option<f64>,
    pub upload_date: String,
    pub description: String,
    pub thumbnail: String,
    pub webpage_url: String,
    pub extractor_key: String,
    pub chapters_json: String,
    pub format_id: String,
    pub resolution: String,
    pub ext: String,
}

fn s(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn f(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(Value::as_f64)
}

pub fn parse_meta(v: &Value) -> Meta {
    let channel = Some(s(v, "channel"))
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| s(v, "uploader"));
    let channel_id = Some(s(v, "channel_id"))
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| s(v, "uploader_id"));
    let chapters_json = match v.get("chapters") {
        Some(c @ Value::Array(a)) if !a.is_empty() => c.to_string(),
        _ => String::new(),
    };
    Meta {
        id: s(v, "id"),
        title: s(v, "title"),
        channel,
        channel_id,
        duration: f(v, "duration"),
        upload_date: s(v, "upload_date"),
        description: s(v, "description"),
        thumbnail: s(v, "thumbnail"),
        webpage_url: Some(s(v, "webpage_url"))
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| s(v, "original_url")),
        extractor_key: s(v, "extractor_key"),
        chapters_json,
        format_id: s(v, "format_id"),
        resolution: s(v, "resolution"),
        ext: s(v, "ext"),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Chapter {
    pub start: f64,
    pub end: f64,
    pub title: String,
}

pub fn parse_chapters(json: &str) -> Vec<Chapter> {
    let Ok(Value::Array(a)) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    a.iter()
        .filter_map(|c| {
            Some(Chapter {
                start: f(c, "start_time")?,
                end: f(c, "end_time").unwrap_or(f64::NAN),
                title: s(c, "title"),
            })
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatKind {
    Muxed,
    VideoOnly,
    AudioOnly,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Format {
    pub id: String,
    pub ext: String,
    pub kind: FormatKind,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    pub vcodec: String,
    pub acodec: String,
    pub size: Option<u64>,
    pub approx: bool,
    pub tbr: Option<f64>,
    pub note: String,
    pub resolution: String,
    pub language: String,
}

impl Format {
    pub fn describe(&self) -> String {
        let mut parts = vec![self.ext.clone()];
        match self.kind {
            FormatKind::AudioOnly => {}
            _ => {
                parts.push(if self.resolution.is_empty() {
                    self.height.map(|h| format!("{h}p")).unwrap_or_default()
                } else {
                    self.resolution.clone()
                });
                if let Some(fps) = self.fps.filter(|f| *f > 0.0) {
                    parts.push(format!("{fps:.0} fps"));
                }
            }
        }
        let codec = match self.kind {
            FormatKind::VideoOnly => short_codec(&self.vcodec),
            FormatKind::AudioOnly => short_codec(&self.acodec),
            FormatKind::Muxed => format!(
                "{}+{}",
                short_codec(&self.vcodec),
                short_codec(&self.acodec)
            ),
        };
        parts.push(codec);
        if let Some(t) = self.tbr {
            parts.push(format!("{t:.0}k"));
        }
        if !self.note.is_empty() {
            parts.push(self.note.clone());
        }
        if !self.language.is_empty() && self.kind != FormatKind::VideoOnly {
            parts.push(format!("[{}]", self.language));
        }
        parts.retain(|p| !p.is_empty());
        parts.join(" · ")
    }
}

fn short_codec(c: &str) -> String {
    c.split('.').next().unwrap_or(c).to_owned()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: String,
    pub url: String,
    pub title: String,
    pub channel: String,
    pub duration: Option<f64>,
    /// A nested playlist (such as a channel tab) rather than a video.
    pub is_playlist: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Analysis {
    Video {
        meta: Meta,
        formats: Vec<Format>,
    },
    Playlist {
        title: String,
        channel: String,
        entries: Vec<Entry>,
    },
}

pub fn parse_formats(v: &Value) -> Vec<Format> {
    let Some(Value::Array(list)) = v.get("formats") else {
        return Vec::new();
    };
    let mut out: Vec<Format> = list
        .iter()
        .filter_map(|x| {
            let vcodec = s(x, "vcodec");
            let acodec = s(x, "acodec");
            let has_v = !vcodec.is_empty() && vcodec != "none";
            let has_a = !acodec.is_empty() && acodec != "none";
            let note = s(x, "format_note");
            if note.contains("storyboard") || s(x, "ext") == "mhtml" {
                return None;
            }
            let kind = match (has_v, has_a) {
                (true, true) => FormatKind::Muxed,
                (true, false) => FormatKind::VideoOnly,
                (false, true) => FormatKind::AudioOnly,
                // Unknown codecs: treat as muxed (common for direct links).
                (false, false) => FormatKind::Muxed,
            };
            let size = x.get("filesize").and_then(Value::as_u64);
            let approx_size = x.get("filesize_approx").and_then(Value::as_u64);
            Some(Format {
                id: s(x, "format_id"),
                ext: s(x, "ext"),
                kind,
                height: x.get("height").and_then(Value::as_u64).map(|h| h as u32),
                fps: f(x, "fps"),
                vcodec,
                acodec,
                size: size.or(approx_size),
                approx: size.is_none() && approx_size.is_some(),
                tbr: f(x, "tbr"),
                note,
                resolution: s(x, "resolution"),
                language: s(x, "language"),
            })
        })
        .filter(|f| !f.id.is_empty())
        .collect();
    // Best first, as people expect.
    out.reverse();
    out
}

pub fn parse_analysis(v: &Value) -> Analysis {
    let is_playlist = matches!(v.get("_type").and_then(Value::as_str), Some("playlist"))
        || v.get("entries").is_some_and(Value::is_array);
    if is_playlist {
        let entries = v
            .get("entries")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(parse_entry).collect())
            .unwrap_or_default();
        return Analysis::Playlist {
            title: s(v, "title"),
            channel: Some(s(v, "channel"))
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| s(v, "uploader")),
            entries,
        };
    }
    Analysis::Video {
        meta: parse_meta(v),
        formats: parse_formats(v),
    }
}

/// One `--flat-playlist` entry (from `-J` entries or `-j` lines).
pub fn parse_entry(e: &Value) -> Option<Entry> {
    if e.is_null() {
        return None;
    }
    let id = s(e, "id");
    let mut url = s(e, "url");
    if url.is_empty() {
        url = s(e, "webpage_url");
    }
    if url.is_empty() {
        return None;
    }
    let ie = s(e, "ie_key");
    let is_playlist = s(e, "_type") == "playlist"
        || (ie == "YoutubeTab" && !url.contains("watch?v="))
        || e.get("entries").is_some();
    Some(Entry {
        id,
        url,
        title: Some(s(e, "title"))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| s(e, "url")),
        channel: Some(s(e, "channel"))
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| s(e, "uploader")),
        duration: f(e, "duration"),
        is_playlist,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn format_specs() {
        let mut o = DownloadOptions::default();
        assert_eq!(o.format_spec(), "bv*+ba/b/bv*+ba/b");
        o.max_height = 720;
        assert_eq!(
            o.format_spec(),
            "bv*[height<=720]+ba/b[height<=720]/bv*+ba/b"
        );
        o.mode = Mode::AudioOnly;
        assert_eq!(o.format_spec(), "ba/b");
        o.format = " 137+140 ".into();
        assert_eq!(o.format_spec(), "137+140");
        assert_eq!(o.expected_parts("137+140"), 2);
        assert_eq!(o.expected_parts(""), 1);
    }

    #[test]
    fn custom_arg_validation() {
        assert_eq!(
            validate_custom_args("--sponsorblock-remove all -N 4").unwrap(),
            vec!["--sponsorblock-remove", "all", "-N", "4"]
        );
        assert!(validate_custom_args("--print title").is_err());
        assert!(validate_custom_args("--progress-template=x").is_err());
        assert!(validate_custom_args("https://x").is_err());
        assert!(validate_custom_args("--x 'unterminated").is_err());
        assert!(validate_custom_args("").unwrap().is_empty());
        assert!(
            validate_custom_args("--simul").is_err(),
            "prefix of --simulate"
        );
        assert!(validate_custom_args("--qui").is_err());
        assert!(validate_custom_args("-sj").is_err(), "short cluster");
        assert!(validate_custom_args("-Otitle").is_err());
        assert!(validate_custom_args("-N 4 -- https://x").is_err());
        assert!(validate_custom_args("-N 4 https://x").is_err());
        assert!(validate_custom_args("--proxy socks5://127.0.0.1:9050").is_ok());
        assert!(validate_custom_args("-f bv+ba --print-traffic").is_ok());
        assert!(
            validate_custom_args("-fbestaudio").is_ok(),
            "value after -f"
        );
        assert!(check_imported_args("-N 4 --sponsorblock-remove all --embed-subs").is_ok());
        assert!(check_imported_args("-N4").is_ok());
        assert!(check_imported_args("--exec 'x'").is_err());
        assert!(
            check_imported_args("--exe 'x'").is_err(),
            "prefixes are refused"
        );
        assert!(check_imported_args("--alias foo --exec").is_err());
        assert!(check_imported_args("--external-downloader aria2c").is_err());
        assert!(check_imported_args("--plugin-dirs /x").is_err());
        assert!(check_imported_args("--postprocessor-args 'x'").is_err());
        assert!(check_imported_args("-P /etc").is_err());
        assert!(check_imported_args("--print x").is_err(), "reserved too");
        assert!(check_imported_args("").is_ok());
        let v = validation_args(&["-N".into(), "4".into()]);
        assert_eq!(v.last().unwrap(), VALIDATION_URL);
        assert_eq!(
            validation_result(
                Some(2),
                "Usage: yt-dlp [OPTIONS] URL

yt-dlp: error: no such option: --x
"
            ),
            Err("no such option: --x".into())
        );
        assert!(validation_result(Some(1), "ERROR: 'beardl:validate' is not a valid URL").is_ok());
    }

    fn job_args(o: &DownloadOptions) -> Vec<String> {
        let dir = PathBuf::from("/dl");
        download_args(&Job {
            url: "https://example.com/v",
            options: o,
            download_dir: &dir,
            output_template: "%(title)s.%(ext)s",
            sub_langs: "en",
            rate_limit: Some(1000),
            ffmpeg_dir: Some(Path::new("/ff")),
            thumbs_dir: Path::new("/thumbs"),
            subs_dir: Path::new("/subs"),
            date_after: Some("20240101"),
            has_ffmpeg: true,
        })
    }

    fn has_pair(v: &[String], a: &str, b: &str) -> bool {
        v.windows(2).any(|w| w[0] == a && w[1] == b)
    }

    #[test]
    fn builds_download_args() {
        let mut o = DownloadOptions {
            container: "mkv".into(),
            custom_args: "-N 4".into(),
            folder: "music".into(),
            ..Default::default()
        };
        let v = job_args(&o);
        assert_eq!(v.last().unwrap(), "https://example.com/v");
        assert_eq!(v[v.len() - 2], "--");
        assert!(has_pair(&v, "--merge-output-format", "mkv"));
        assert!(has_pair(&v, "--limit-rate", "1000"));
        assert!(has_pair(&v, "--dateafter", "20240101"));
        assert!(has_pair(&v, "--ffmpeg-location", "/ff"));
        assert!(has_pair(&v, "-N", "4"));
        assert!(has_pair(&v, "--sub-langs", "en"));
        assert!(v.iter().any(|a| a == "--embed-chapters"));
        let p = v.iter().position(|a| a == "-P").unwrap();
        assert!(v[p + 1].ends_with("music"));

        o.mode = Mode::AudioOnly;
        o.audio_format = "mp3".into();
        o.subtitles = false;
        o.auto_subtitles = false;
        o.custom_args = "--print x".into();
        let v = job_args(&o);
        assert!(has_pair(&v, "--audio-format", "mp3"));
        assert!(!v.iter().any(|a| a == "--merge-output-format"));
        assert!(!v.iter().any(|a| a == "--write-subs"));
        assert!(
            !v.iter().any(|a| a == "x"),
            "invalid custom args are dropped"
        );
    }

    #[test]
    fn parses_progress_lines() {
        let l = parse_line("[bdl]downloading|1024|NA|4096|512.5|6|137\r\n");
        let Line::Progress(p) = l else {
            panic!("{l:?}")
        };
        assert_eq!(p.downloaded, Some(1024));
        assert_eq!(p.total, Some(4096));
        assert_eq!(p.speed, Some(512.5));
        assert_eq!(p.eta, Some(6.0));
        assert_eq!(p.format_id, "137");
        let Line::Progress(p) = parse_line("[bdl]downloading|NA|NA|NA|NA|NA|NA") else {
            panic!()
        };
        assert_eq!(p.downloaded, None);
        assert_eq!(parse_line("[bdl]short|1"), Line::Other);
        assert_eq!(
            parse_line("[bdlpp]started|Merger"),
            Line::PostProcess {
                status: "started".into(),
                name: "Merger".into()
            }
        );
        assert_eq!(
            parse_line("[bdlfile]C:\\x\\a b.mp4"),
            Line::File("C:\\x\\a b.mp4".into())
        );
        assert_eq!(
            parse_line("ERROR: [youtube] x: Video unavailable"),
            Line::Error("[youtube] x: Video unavailable".into())
        );
        assert!(matches!(
            parse_line(r#"[bdlmeta]{"id":"x","title":"T"}"#),
            Line::Meta(_)
        ));
        assert_eq!(parse_line("[bdlmeta]{broken"), Line::Other);
        assert!(is_filter_skip(
            "[download] upload date is not in range 20240101 - 99991231"
        ));
    }

    #[test]
    fn parses_meta_and_chapters() {
        let v = json!({
            "id": "abc", "title": "T", "uploader": "U", "duration": 61.5,
            "upload_date": "20240102", "extractor_key": "Youtube",
            "chapters": [{"start_time": 0.0, "end_time": 30.0, "title": "Intro"},
                         {"start_time": 30.0, "end_time": 61.5, "title": "Main"}],
            "format_id": "137+140"
        });
        let m = parse_meta(&v);
        assert_eq!(m.channel, "U");
        assert_eq!(m.duration, Some(61.5));
        let ch = parse_chapters(&m.chapters_json);
        assert_eq!(ch.len(), 2);
        assert_eq!(ch[1].title, "Main");
        assert!(parse_chapters("").is_empty());
        assert!(
            parse_meta(&json!({"chapters": []}))
                .chapters_json
                .is_empty()
        );
    }

    #[test]
    fn parses_analysis() {
        let video = json!({
            "id": "v", "title": "Video",
            "formats": [
                {"format_id": "sb0", "ext": "mhtml", "format_note": "storyboard", "vcodec": "none", "acodec": "none"},
                {"format_id": "140", "ext": "m4a", "vcodec": "none", "acodec": "mp4a.40.2", "filesize": 1000, "language": "en"},
                {"format_id": "137", "ext": "mp4", "vcodec": "avc1.640028", "acodec": "none", "height": 1080, "fps": 30, "filesize_approx": 5000},
                {"format_id": "18", "ext": "mp4", "vcodec": "avc1", "acodec": "mp4a", "height": 360}
            ]
        });
        let Analysis::Video { formats, .. } = parse_analysis(&video) else {
            panic!()
        };
        assert_eq!(formats.len(), 3);
        assert_eq!(formats[0].id, "18");
        assert_eq!(formats[0].kind, FormatKind::Muxed);
        assert_eq!(formats[1].kind, FormatKind::VideoOnly);
        assert!(formats[1].approx);
        assert_eq!(formats[2].kind, FormatKind::AudioOnly);
        assert!(formats[1].describe().contains("30 fps"));

        let pl = json!({
            "_type": "playlist", "title": "List", "uploader": "Chan",
            "entries": [
                {"id": "a", "url": "https://www.youtube.com/watch?v=a", "title": "A", "duration": 10, "ie_key": "Youtube"},
                null,
                {"id": "UCx", "url": "https://www.youtube.com/channel/UCx/shorts", "title": "Shorts", "ie_key": "YoutubeTab", "_type": "url"},
                {"id": "nourl"}
            ]
        });
        let Analysis::Playlist {
            entries, channel, ..
        } = parse_analysis(&pl)
        else {
            panic!()
        };
        assert_eq!(channel, "Chan");
        assert_eq!(entries.len(), 2);
        assert!(!entries[0].is_playlist);
        assert!(entries[1].is_playlist);
    }
}
