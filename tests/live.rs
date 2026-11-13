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
