//! End-to-end checks against the real yt-dlp and network. Run with
//! `cargo test --test live -- --ignored --nocapture`.
use beardl::db::{Db, Status};
use beardl::engine::{Cmd, Engine, EngineConfig, NewItem};
use beardl::settings::Settings;
use beardl::ytdlp::{Analysis, DownloadOptions, Mode};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

const VIDEO: &str = "https://www.youtube.com/watch?v=jNQXAC9IVRw";
const DIRECT: &str = "https://www.w3schools.com/html/mov_bbb.mp4";

fn home(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("beardl-live-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn start(h: &std::path::Path) -> Engine {
    let settings = Settings {
        download_dir: h.join("downloads"),
        auto_update: false,
        ..Default::default()
    };
    for d in ["tools", "thumbs", "subs"] {
        std::fs::create_dir_all(h.join(d)).unwrap();
    }
    Engine::start(
        EngineConfig {
            settings,
            tools_dir: h.join("tools"),
            db_path: h.join("library.sqlite"),
            thumbs_dir: h.join("thumbs"),
            subs_dir: h.join("subs"),
            themes_dir: h.join("themes"),
        },
        Arc::new(|_| {}),
    )
}

fn wait(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    panic!("timed out waiting for {what}");
}

#[test]
#[ignore]
fn analyse_and_download() {
    let h = home("dl");
    let engine = start(&h);
    wait("yt-dlp install", 180, || {
        engine.state().paths.ytdlp.is_some()
    });

    engine.send(Cmd::Analyse(VIDEO.into()));
    wait("analysis", 120, || {
        engine
            .state()
            .analysis
            .as_ref()
            .is_some_and(|a| a.result.is_some())
    });
    let analysis = engine.state().analysis.clone().unwrap().result.unwrap();
    match analysis.as_deref() {
        Ok(Analysis::Video { meta, formats }) => {
            eprintln!("analysed: {} ({} formats)", meta.title, formats.len());
            assert!(!formats.is_empty());
        }
        other => panic!("unexpected analysis: {other:?}"),
    }

    for (url, mode) in [(VIDEO, Mode::AudioOnly), (DIRECT, Mode::VideoAudio)] {
        engine.send(Cmd::Enqueue(vec![NewItem {
            url: url.into(),
            title: url.into(),
            preset: "test".into(),
            options: DownloadOptions {
                mode,
                audio_format: if mode == Mode::AudioOnly {
                    "m4a".into()
                } else {
                    String::new()
                },
                ..Default::default()
            },
            ..Default::default()
        }]));
    }
    wait("downloads", 300, || {
        let st = engine.state();
        st.queue.len() == 2
            && st
                .queue
                .iter()
                .all(|q| !matches!(q.status, Status::Queued | Status::Running))
    });
    let queue = engine.state().queue.clone();
    drop(engine);
    for q in &queue {
        eprintln!("{} -> {:?} {} {}", q.url, q.status, q.error, q.file_path);
    }
    let db = Db::open(&h.join("library.sqlite")).unwrap();
    let direct = queue.iter().find(|q| q.url == DIRECT).unwrap();
    assert_eq!(direct.status, Status::Done, "{}", direct.error);
    let d = db.get_download(direct.id).unwrap().unwrap();
    assert!(std::path::Path::new(&d.file_path).is_file());
    assert!(d.file_size.unwrap_or(0) > 0);
    let yt = queue.iter().find(|q| q.url == VIDEO).unwrap();
    if yt.status == Status::Done {
        let d = db.get_download(yt.id).unwrap().unwrap();
        assert!(d.file_path.ends_with(".m4a"), "{}", d.file_path);
        assert_eq!(d.video_id, "jNQXAC9IVRw");
        assert!(!d.title.is_empty());
        assert!(d.duration.is_some());
        assert!(!d.thumb_path.is_empty(), "thumbnail cached");
        // Captions were downloaded and every line indexed for spoken-word search.
        let (videos, tracks) = db.cue_stats().unwrap();
        eprintln!("indexed subtitle tracks: {tracks} in {videos} videos");
        assert!(tracks > 0, "subtitles indexed");
        let hits = db
            .search_cues(&beardl::subtitles::cue_query("elephants").unwrap(), 10)
            .unwrap();
        eprintln!(
            "hits for 'elephants': {:?}",
            hits.iter()
                .map(|h| (h.start, &h.snippet))
                .collect::<Vec<_>>()
        );
        assert!(hits.iter().any(|h| h.download_id == yt.id));
    } else {
        eprintln!("YouTube download did not complete: {}", yt.error);
    }
}

#[test]
#[ignore]
fn pause_resume_cancel() {
    let h = home("ctl");
    let settings = Settings {
        download_dir: h.join("downloads"),
        auto_update: false,
        rate_limit: "40K".into(),
        concurrency: 2,
        ..Default::default()
    };
    for d in ["tools", "thumbs", "subs"] {
        std::fs::create_dir_all(h.join(d)).unwrap();
    }
    let engine = Engine::start(
        EngineConfig {
            settings,
            tools_dir: h.join("tools"),
            db_path: h.join("library.sqlite"),
            thumbs_dir: h.join("thumbs"),
            subs_dir: h.join("subs"),
            themes_dir: h.join("themes"),
        },
        Arc::new(|_| {}),
    );
    wait("yt-dlp install", 180, || {
        engine.state().paths.ytdlp.is_some()
    });
    let item = |folder: &str| NewItem {
        url: DIRECT.into(),
        title: folder.into(),
        options: DownloadOptions {
            folder: folder.into(),
            ..Default::default()
        },
        ..Default::default()
    };
    engine.send(Cmd::Enqueue(vec![item("a"), item("b")]));
    wait("both running with progress", 120, || {
        let st = engine.state();
        st.queue.len() == 2
            && st
                .queue
                .iter()
                .all(|q| q.status == Status::Running && q.progress.fraction.unwrap_or(0.0) > 0.0)
    });
    let ids: Vec<i64> = engine.state().queue.iter().map(|q| q.id).collect();
    let speed = engine.state().queue[0].progress.speed;
    eprintln!("speed while limited: {speed:?}");

    engine.send(Cmd::Pause(ids[0]));
    engine.send(Cmd::Cancel(ids[1]));
    wait("pause and cancel", 30, || {
        let st = engine.state();
        st.queue[0].status == Status::Paused && st.queue[1].status == Status::Cancelled
    });
    std::thread::sleep(Duration::from_secs(2));
    let leftovers: Vec<_> = std::fs::read_dir(h.join("downloads/b"))
        .map(|r| r.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(leftovers.is_empty(), "partial files left: {leftovers:?}");
    let partial: Vec<_> = std::fs::read_dir(h.join("downloads/a"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(partial.iter().any(|n| n.ends_with(".part")), "{partial:?}");

    engine.send(Cmd::Resume(ids[0]));
    wait("resumed download", 120, || {
        engine.state().queue[0].status == Status::Done
    });
    let done = engine.state().queue[0].file_path.clone();
    assert!(std::path::Path::new(&done).is_file());
    assert!(!std::path::Path::new(&format!("{done}.part")).exists());
}

#[test]
#[ignore]
fn validates_custom_args_with_ytdlp() {
    let h = home("val");
    let engine = start(&h);
    wait("yt-dlp install", 180, || {
        engine.state().paths.ytdlp.is_some()
    });
    for (args, ok) in [
        ("-N 4 --sponsorblock-remove all", true),
        ("--no-such-flag", false),
    ] {
        engine.send(Cmd::ValidateArgs(args.into()));
        wait(
            "validation",
            60,
            || matches!(&engine.state().validation, Some((a, Some(_))) if a == args),
        );
        let res = engine.state().validation.clone().unwrap().1.unwrap();
        eprintln!("{args}: {res:?}");
        assert_eq!(res.is_ok(), ok);
    }
}

#[test]
#[ignore]
fn immediate_resume_after_pause_completes() {
    let h = home("race");
    let settings = Settings {
        download_dir: h.join("downloads"),
        auto_update: false,
        rate_limit: "60K".into(),
        ..Default::default()
    };
    for d in ["tools", "thumbs", "subs"] {
        std::fs::create_dir_all(h.join(d)).unwrap();
    }
    let engine = Engine::start(
        EngineConfig {
            settings,
            tools_dir: h.join("tools"),
            db_path: h.join("library.sqlite"),
            thumbs_dir: h.join("thumbs"),
            subs_dir: h.join("subs"),
            themes_dir: h.join("themes"),
        },
        Arc::new(|_| {}),
    );
    wait("yt-dlp install", 180, || {
        engine.state().paths.ytdlp.is_some()
    });
    engine.send(Cmd::Enqueue(vec![NewItem {
        url: DIRECT.into(),
        title: "race".into(),
        ..Default::default()
    }]));
    wait("running", 60, || {
        engine
            .state()
            .queue
            .first()
            .is_some_and(|q| q.progress.fraction.unwrap_or(0.0) > 0.0)
    });
    let id = engine.state().queue[0].id;
    engine.send(Cmd::Pause(id));
    engine.send(Cmd::Resume(id));
    wait("completion after quick resume", 120, || {
        engine.state().queue[0].status == Status::Done
    });
}

#[test]
#[ignore]
fn subscription_checks_only_queue_new_items() {
    use beardl::subs::SubConfig;
    let h = home("subs");
    let engine = start(&h);
    wait("yt-dlp install", 180, || {
        engine.state().paths.ytdlp.is_some()
    });
    engine.send(Cmd::AddSubscription {
        name: String::new(),
        url: "https://www.youtube.com/@jawed/videos".into(),
        config: SubConfig {
            max_items: 3,
            ..Default::default()
        },
    });
    wait("first check", 120, || {
        engine
            .state()
            .subscriptions
            .first()
            .is_some_and(|v| !v.logs.is_empty() && !v.checking)
    });
    let v = engine.state().subscriptions[0].clone();
    eprintln!("name: {} | log: {}", v.sub.name, v.logs[0].message);
    assert!(!v.logs[0].is_error, "{}", v.logs[0].message);
    assert!(v.seen >= 1);
    let queued: Vec<_> = engine
        .state()
        .queue
        .iter()
        .map(|q| q.title.clone())
        .collect();
    assert!(!queued.is_empty(), "first check queues the newest items");
    wait("download", 300, || {
        engine
            .state()
            .queue
            .iter()
            .all(|q| !matches!(q.status, Status::Queued | Status::Running))
    });
    let notices = engine.state().desktop_notices.clone();
    eprintln!("notices: {notices:?}");
    assert!(
        !notices.is_empty(),
        "a notification is raised for new downloads"
    );

    let before = engine.state().queue.len();
    engine.send(Cmd::CheckSubscription(v.sub.id));
    wait("second check", 120, || {
        let st = engine.state();
        let s = &st.subscriptions[0];
        !s.checking && s.logs.len() >= 2
    });
    let v2 = engine.state().subscriptions[0].clone();
    eprintln!("second: {}", v2.logs[0].message);
    assert!(
        v2.logs[0].message.contains("0 queued"),
        "{}",
        v2.logs[0].message
    );
    assert_eq!(engine.state().queue.len(), before);
}

/// Shows a real tray icon and notification briefly, and adds then removes the start-up entry.
#[test]
#[ignore]
fn tray_notification_and_autostart() {
    let ctx = eframe::egui::Context::default();
    let tray = beardl::platform::Tray::new(&ctx).expect("tray icon");
    tray.notify("BearDL test", "Tray notifications work");
    std::thread::sleep(Duration::from_secs(2));
    drop(tray);
    beardl::platform::set_autostart(true).unwrap();
    #[cfg(windows)]
    {
        let out = std::process::Command::new("reg")
            .args([
                "query",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v",
                "BearDL",
            ])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        eprintln!("{text}");
        assert!(text.contains("--minimised"));
    }
    beardl::platform::set_autostart(false).unwrap();
    beardl::platform::set_autostart(false).unwrap();
}

#[test]
#[ignore]
fn trims_fast_and_accurate() {
    let h = home("trim");
    let engine = start(&h);
    wait("yt-dlp install", 180, || {
        engine.state().paths.ytdlp.is_some()
    });
    engine.send(Cmd::Enqueue(vec![NewItem {
        url: DIRECT.into(),
        title: "trim".into(),
        ..Default::default()
    }]));
    wait("download", 120, || {
        engine
            .state()
            .queue
            .first()
            .is_some_and(|q| q.status == Status::Done)
    });
    let id = engine.state().queue[0].id;
    engine.send(Cmd::Trim {
        id,
        start: 2.0,
        end: 5.0,
        accurate: false,
    });
    engine.send(Cmd::Trim {
        id,
        start: 2.0,
        end: 5.0,
        accurate: true,
    });
    wait("both trims", 120, || {
        let st = engine.state();
        st.trims.len() == 2 && st.trims.iter().all(|t| t.done)
    });
    let trims = engine.state().trims.clone();
    drop(engine);
    for t in &trims {
        assert!(t.error.is_none(), "{:?}", t.error);
    }
    let db = Db::open(&h.join("library.sqlite")).unwrap();
    let q = beardl::db::LibraryQuery {
        text: "clip".into(),
        ..Default::default()
    };
    let rows = db.library_page(&q, 0, 10).unwrap();
    assert_eq!(rows.len(), 2, "both clips are library entries");
    let a = db.get_download(rows[0].id).unwrap().unwrap().file_path;
    let b = db.get_download(rows[1].id).unwrap().unwrap().file_path;
    assert_ne!(a, b, "simultaneous trims write different files");
    let mut durations = Vec::new();
    for r in rows {
        let d = db.get_download(r.id).unwrap().unwrap();
        assert!(std::path::Path::new(&d.file_path).is_file());
        let out = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "format=duration",
                "-of",
                "csv=p=0",
            ])
            .arg(&d.file_path)
            .output()
            .unwrap();
        let dur: f64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
        eprintln!("{} -> {dur:.2}s", d.file_path);
        // Fast cuts start at the keyframe before the start, so they can be longer.
        assert!((2.8..=5.6).contains(&dur), "clip length {dur}");
        assert!(d.title.contains("mov_bbb"));
        let stored = d.duration.unwrap_or(0.0);
        assert!(
            (stored - dur).abs() < 0.3,
            "library length {stored} vs file {dur}"
        );
        durations.push(dur);
    }
    assert!(
        durations.iter().any(|d| (d - 3.0).abs() < 0.2),
        "the accurate clip is exact: {durations:?}"
    );
}
