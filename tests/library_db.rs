//! Integration tests for the library side of `beardl::db`: search, sorting, filters, paging,
//! FTS sync and the queue helpers that share the `downloads` table.

use beardl::db::{Db, Download, LibraryQuery, LibraryRow, SortKey, Status, fts_query};
use std::cmp::Ordering;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const ALL_SORTS: [SortKey; 5] = [
    SortKey::Date,
    SortKey::Title,
    SortKey::Channel,
    SortKey::Duration,
    SortKey::Size,
];

fn dl(title: &str, channel: &str, status: Status) -> Download {
    Download {
        url: format!("https://example.com/{}", title.replace(' ', "_")),
        title: title.into(),
        channel: channel.into(),
        status: Some(status),
        ..Default::default()
    }
}

fn add(db: &Db, title: &str, channel: &str, status: Status) -> i64 {
    db.insert_download(&dl(title, channel, status)).unwrap()
}

fn q(text: &str) -> LibraryQuery {
    LibraryQuery {
        text: text.into(),
        ..Default::default()
    }
}

fn count(db: &Db, text: &str) -> usize {
    db.library_count(&q(text)).unwrap()
}

fn all_rows(db: &Db, query: &LibraryQuery) -> Vec<LibraryRow> {
    db.library_page(query, 0, 1_000_000).unwrap()
}

fn titles(db: &Db, query: &LibraryQuery) -> Vec<String> {
    let mut t: Vec<String> = all_rows(db, query).into_iter().map(|r| r.title).collect();
    t.sort();
    t
}

fn ids(rows: &[LibraryRow]) -> Vec<i64> {
    rows.iter().map(|r| r.id).collect()
}

/// Rows matched by the FTS index directly, bypassing the library filters.
fn fts_hits(db: &Db, fts: &str) -> i64 {
    db.conn
        .query_row(
            "SELECT COUNT(*) FROM downloads_fts WHERE downloads_fts MATCH ?1",
            [fts],
            |r| r.get(0),
        )
        .unwrap()
}

fn fts_integrity(db: &Db) {
    db.conn
        .execute(
            "INSERT INTO downloads_fts(downloads_fts, rank) VALUES ('integrity-check', 1)",
            [],
        )
        .expect("FTS index out of sync with downloads");
}

// ---------------------------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------------------------

#[test]
fn fts_query_builds_quoted_prefix_terms() {
    assert_eq!(fts_query(""), None);
    assert_eq!(fts_query("   \t\n "), None);
    assert_eq!(fts_query("\""), None);
    assert_eq!(fts_query("\"\" \""), None);
    assert_eq!(fts_query("rust").as_deref(), Some("\"rust\"*"));
    assert_eq!(
        fts_query("  rust   lang ").as_deref(),
        Some("\"rust\"* \"lang\"*")
    );
    assert_eq!(fts_query("ru\"st").as_deref(), Some("\"rust\"*"));
}

#[test]
fn search_special_characters_do_not_error() {
    let db = Db::open_memory().unwrap();
    add(&db, "Tom AND Jerry", "Toons", Status::Done);
    add(&db, "Nearby places", "Travel", Status::Done);
    add(&db, "Rust: the book (2nd ed)", "Readers", Status::Done);
    add(&db, "C++ - a tour", "Langs", Status::Done);
    let queries = [
        "\"",
        "\"\"",
        "\"rust",
        "rust\"",
        "*",
        "**",
        "rust*",
        "*rust",
        "-",
        "-rust",
        "rust -",
        ":",
        "title:",
        "title:rust",
        "channel:readers",
        "(",
        ")",
        "(rust",
        "rust)",
        "()",
        "AND",
        "OR",
        "NOT",
        "rust AND",
        "AND rust",
        "NEAR",
        "NEAR(",
        "NEAR(rust book)",
        "NEAR(rust book, 2)",
        "^",
        "^rust",
        "+",
        "{title}",
        "[",
        "'",
        "\\",
        "%",
        "_",
        ";",
        "rust; DROP TABLE downloads; --",
        "\u{1}",
        "\u{7f}",
        "rust\u{200b}",
        "🦀",
        "é",
    ];
    for text in queries {
        let query = q(text);
        let n = db
            .library_count(&query)
            .unwrap_or_else(|e| panic!("library_count({text:?}) failed: {e}"));
        let rows = db
            .library_page(&query, 0, 50)
            .unwrap_or_else(|e| panic!("library_page({text:?}) failed: {e}"));
        assert_eq!(rows.len(), n, "count and page disagree for {text:?}");
    }
    // Operators are treated as plain words, not FTS syntax.
    assert_eq!(titles(&db, &q("AND")), vec!["Tom AND Jerry"]);
    assert_eq!(titles(&db, &q("and jerry")), vec!["Tom AND Jerry"]);
    assert_eq!(titles(&db, &q("NEAR")), vec!["Nearby places"]);
    assert_eq!(titles(&db, &q("rust:")), vec!["Rust: the book (2nd ed)"]);
    assert_eq!(titles(&db, &q("(2nd")), vec!["Rust: the book (2nd ed)"]);
    assert_eq!(titles(&db, &q("c++")), vec!["C++ - a tour"]);
    // A column filter is not honoured: "title:rust" is the phrase "title rust", which no row has.
    assert_eq!(count(&db, "title:rust"), 0);
    // The table still exists after the "injection".
    assert_eq!(count(&db, ""), 4);
}

#[test]
fn search_with_nul_character_does_not_error() {
    let db = Db::open_memory().unwrap();
    add(&db, "Rust tutorial", "Alpha", Status::Done);
    for text in ["\u{0}", "rust\u{0}", "\u{0}rust", "rust \u{0} tut"] {
        let query = q(text);
        db.library_count(&query)
            .unwrap_or_else(|e| panic!("library_count({text:?}) failed: {e}"));
        db.library_page(&query, 0, 50)
            .unwrap_or_else(|e| panic!("library_page({text:?}) failed: {e}"));
    }
    assert_eq!(count(&db, "rust\u{0}"), 1);
}

#[test]
fn search_ignores_punctuation_only_words() {
    let db = Db::open_memory().unwrap();
    add(&db, "Rust & Go", "Langs", Status::Done);
    add(&db, "C++ - a tour", "Langs", Status::Done);
    // Words that contain no indexable characters must not wipe out the whole result.
    assert_eq!(titles(&db, &q("rust & go")), vec!["Rust & Go"]);
    assert_eq!(titles(&db, &q("c++ - tour")), vec!["C++ - a tour"]);
    assert_eq!(count(&db, "-"), 0);
}

#[test]
fn search_unicode_and_diacritics() {
    let db = Db::open_memory().unwrap();
    add(&db, "Paris cafe walk", "Walks", Status::Done);
    add(&db, "Rustic café tour", "Walks", Status::Done);
    add(&db, "Ångström units", "Science", Status::Done);
    add(&db, "Ελληνικά μαθήματα", "Γλώσσες", Status::Done);
    add(&db, "東京 travel", "旅", Status::Done);

    let both = vec!["Paris cafe walk", "Rustic café tour"];
    assert_eq!(titles(&db, &q("café")), both);
    assert_eq!(titles(&db, &q("cafe")), both);
    assert_eq!(titles(&db, &q("CAFÉ")), both);
    assert_eq!(titles(&db, &q("cafè")), both);
    assert_eq!(titles(&db, &q("angstrom")), vec!["Ångström units"]);
    assert_eq!(titles(&db, &q("ÅNGSTRÖM")), vec!["Ångström units"]);
    // Case folding works beyond Latin; diacritic removal in unicode61 is Latin-only by design,
    // so Greek accents must be typed.
    assert_eq!(titles(&db, &q("ΕΛΛΗΝΙΚΆ")), vec!["Ελληνικά μαθήματα"]);
    assert_eq!(titles(&db, &q("γλώσσες")), vec!["Ελληνικά μαθήματα"]);
    assert_eq!(titles(&db, &q("東京")), vec!["東京 travel"]);
}

#[test]
fn search_prefix_matching() {
    let db = Db::open_memory().unwrap();
    add(&db, "Rust tutorial", "Alpha", Status::Done);
    add(&db, "Rustic cabin", "Alpha", Status::Done);
    add(&db, "Trust issues", "Alpha", Status::Done);
    add(&db, "Tutorials for rustaceans", "Beta", Status::Done);

    assert_eq!(
        titles(&db, &q("rus")),
        vec!["Rust tutorial", "Rustic cabin", "Tutorials for rustaceans"]
    );
    // Prefix, not substring.
    assert_eq!(count(&db, "ust"), 0);
    assert_eq!(titles(&db, &q("trust")), vec!["Trust issues"]);
    // Every word must match (implicit AND), each as a prefix, in any order.
    assert_eq!(
        titles(&db, &q("tut rus")),
        vec!["Rust tutorial", "Tutorials for rustaceans"]
    );
    assert_eq!(titles(&db, &q("rustic cab")), vec!["Rustic cabin"]);
    assert_eq!(count(&db, "rustic tut"), 0);
    // The whole word still matches itself.
    assert_eq!(count(&db, "tutorial"), 2);
    assert_eq!(count(&db, "tutorials"), 1);
}

#[test]
fn search_matches_channel_and_description() {
    let db = Db::open_memory().unwrap();
    let a = add(&db, "Episode one", "Kurzgesagt", Status::Done);
    let b = add(&db, "Episode two", "Veritasium", Status::Done);
    let mut d = db.get_download(b).unwrap().unwrap();
    d.description = "We explore entropy and thermodynamics".into();
    db.update_meta(b, &d).unwrap();

    assert_eq!(ids(&all_rows(&db, &q("kurz"))), vec![a]);
    assert_eq!(ids(&all_rows(&db, &q("veritasium"))), vec![b]);
    assert_eq!(ids(&all_rows(&db, &q("thermo"))), vec![b]);
    assert_eq!(ids(&all_rows(&db, &q("entropy episode"))), vec![b]);
    // Words may come from different columns.
    assert_eq!(ids(&all_rows(&db, &q("kurz one"))), vec![a]);
    assert_eq!(count(&db, "kurz entropy"), 0);
    assert_eq!(count(&db, "episode"), 2);
}

#[test]
fn search_empty_or_whitespace_returns_everything() {
    let db = Db::open_memory().unwrap();
    add(&db, "One", "A", Status::Done);
    add(&db, "Two", "B", Status::Failed);
    add(&db, "Three", "C", Status::Cancelled);
    add(&db, "Queued one", "D", Status::Queued);
    for text in ["", " ", "   ", "\t", "\n\r\t ", "\u{3000}", "\"", "\" \""] {
        assert_eq!(count(&db, text), 3, "query {text:?}");
        assert_eq!(all_rows(&db, &q(text)).len(), 3, "query {text:?}");
    }
}

#[test]
fn search_never_returns_queued_rows() {
    let db = Db::open_memory().unwrap();
    add(&db, "Rust done", "A", Status::Done);
    add(&db, "Rust waiting", "A", Status::Queued);
    assert_eq!(titles(&db, &q("rust")), vec!["Rust done"]);
    // The FTS index itself does contain the queued row.
    assert_eq!(fts_hits(&db, "\"rust\"*"), 2);
}

// ---------------------------------------------------------------------------------------------
// Sorting
// ---------------------------------------------------------------------------------------------

struct Spec {
    title: &'static str,
    channel: &'static str,
    created_at: i64,
    duration: Option<f64>,
    size: Option<i64>,
}

/// Rows with deliberate ties on every sort column and NULL durations/sizes.
fn sort_fixture() -> (Db, Vec<(i64, Spec)>) {
    let specs = vec![
        Spec {
            title: "banana",
            channel: "Zed",
            created_at: 300,
            duration: Some(60.0),
            size: Some(500),
        },
        Spec {
            title: "Apple",
            channel: "alpha",
            created_at: 100,
            duration: None,
            size: Some(100),
        },
        Spec {
            title: "apple",
            channel: "Alpha",
            created_at: 100,
            duration: Some(60.0),
            size: None,
        },
        Spec {
            title: "Cherry",
            channel: "beta",
            created_at: 200,
            duration: Some(5.5),
            size: Some(100),
        },
        Spec {
            title: "banana",
            channel: "ALPHA",
            created_at: 300,
            duration: None,
            size: None,
        },
        Spec {
            title: "date",
            channel: "Zed",
            created_at: 50,
            duration: Some(3600.0),
            size: Some(9_000_000_000),
        },
        Spec {
            title: "Éclair",
            channel: "beta",
            created_at: 400,
            duration: Some(0.0),
            size: Some(0),
        },
        Spec {
            title: "apple",
            channel: "zed",
            created_at: 200,
            duration: None,
            size: Some(500),
        },
        Spec {
            title: "",
            channel: "",
            created_at: 100,
            duration: Some(5.5),
            size: None,
        },
    ];
    let db = Db::open_memory().unwrap();
    // Queued rows interleaved with the library rows must never show up.
    add(&db, "queued first", "Zed", Status::Queued);
    let statuses = [
        Status::Done,
        Status::Failed,
        Status::Cancelled,
        Status::Skipped,
    ];
    let mut out = Vec::new();
    for (i, s) in specs.into_iter().enumerate() {
        let mut d = dl(s.title, s.channel, statuses[i % statuses.len()]);
        d.created_at = s.created_at;
        d.duration = s.duration;
        let id = db.insert_download(&d).unwrap();
        db.set_file(id, &format!("C:/v/{i}.mp4"), s.size).unwrap();
        if i == 3 {
            add(&db, "queued middle", "alpha", Status::Queued);
        }
        out.push((id, s));
    }
    (db, out)
}

/// ASCII-only case folding, like SQLite's NOCASE collation.
fn nocase(s: &str) -> String {
    s.to_ascii_lowercase()
}

fn expected_order(rows: &[(i64, Spec)], sort: SortKey, descending: bool) -> Vec<i64> {
    let mut v: Vec<&(i64, Spec)> = rows.iter().collect();
    v.sort_by(|(ia, a), (ib, b)| {
        let primary = match sort {
            SortKey::Date => a.created_at.cmp(&b.created_at),
            SortKey::Title => nocase(a.title).cmp(&nocase(b.title)),
            SortKey::Channel => nocase(a.channel).cmp(&nocase(b.channel)),
            // SQLite sorts NULL before any value; Option does the same.
            SortKey::Duration => a.duration.partial_cmp(&b.duration).unwrap(),
            SortKey::Size => a.size.cmp(&b.size),
        };
        let ord = primary.then(ia.cmp(ib));
        if descending { ord.reverse() } else { ord }
    });
    v.into_iter().map(|(id, _)| *id).collect()
}

#[test]
fn sort_by_every_key_both_directions_is_deterministic() {
    let (db, rows) = sort_fixture();
    for sort in ALL_SORTS {
        for descending in [false, true] {
            let query = LibraryQuery {
                sort,
                descending,
                ..Default::default()
            };
            let expected = expected_order(&rows, sort, descending);
            let got = ids(&all_rows(&db, &query));
            assert_eq!(got, expected, "{sort:?} descending={descending}");
            // Same answer on a second run.
            assert_eq!(ids(&all_rows(&db, &query)), expected);
            // Paging one row at a time walks the same order.
            let paged: Vec<i64> = (0..expected.len())
                .flat_map(|o| ids(&db.library_page(&query, o, 1).unwrap()))
                .collect();
            assert_eq!(paged, expected, "{sort:?} descending={descending} paged");
            // Ascending and descending are exact mirrors because ties use id in the same direction.
            let mut rev = expected_order(&rows, sort, !descending);
            rev.reverse();
            assert_eq!(rev, expected);
        }
    }
}

#[test]
fn sort_nulls_first_ascending_last_descending() {
    let (db, rows) = sort_fixture();
    let null_dur = rows.iter().filter(|(_, s)| s.duration.is_none()).count();
    let null_size = rows.iter().filter(|(_, s)| s.size.is_none()).count();
    for (sort, nulls) in [(SortKey::Duration, null_dur), (SortKey::Size, null_size)] {
        let field = |r: &LibraryRow| match sort {
            SortKey::Duration => r.duration.is_none(),
            _ => r.file_size.is_none(),
        };
        let asc = all_rows(
            &db,
            &LibraryQuery {
                sort,
                ..Default::default()
            },
        );
        assert!(asc[..nulls].iter().all(field), "{sort:?} asc");
        assert!(!asc[nulls..].iter().any(field), "{sort:?} asc");
        let desc = all_rows(
            &db,
            &LibraryQuery {
                sort,
                descending: true,
                ..Default::default()
            },
        );
        let split = desc.len() - nulls;
        assert!(!desc[..split].iter().any(field), "{sort:?} desc");
        assert!(desc[split..].iter().all(field), "{sort:?} desc");
    }
}

#[test]
fn sort_values_round_trip() {
    let (db, rows) = sort_fixture();
    let got = all_rows(&db, &LibraryQuery::default());
    for (id, s) in &rows {
        let r = got.iter().find(|r| r.id == *id).unwrap();
        assert_eq!(r.title, s.title);
        assert_eq!(r.channel, s.channel);
        assert_eq!(r.created_at, s.created_at);
        assert_eq!(r.duration, s.duration);
        assert_eq!(r.file_size, s.size);
        assert!(!r.missing);
    }
}

// ---------------------------------------------------------------------------------------------
// Filters
// ---------------------------------------------------------------------------------------------

fn filter_fixture() -> (Db, Vec<i64>) {
    let mut db = Db::open_memory().unwrap();
    let rows = [
        ("Rust intro", "Rust Channel", Status::Done),
        ("Rust advanced", "rust channel", Status::Done),
        ("Rust failed", "RUST CHANNEL", Status::Failed),
        ("Cooking", "Kitchen", Status::Done),
        ("Cooking failed", "Kitchen", Status::Failed),
        ("Skipped rust", "Kitchen", Status::Skipped),
        ("Cancelled", "Rust Channel", Status::Cancelled),
        ("Waiting rust", "Rust Channel", Status::Queued),
        ("Running rust", "Rust Channel", Status::Running),
        ("Paused", "Kitchen", Status::Paused),
    ];
    let ids: Vec<i64> = rows.iter().map(|(t, c, s)| add(&db, t, c, *s)).collect();
    // Rust intro, Cooking failed, Waiting rust are missing on disk.
    db.set_missing(&[
        (ids[0], true),
        (ids[4], true),
        (ids[7], true),
        (ids[3], false),
    ])
    .unwrap();
    (db, ids)
}

fn filtered(db: &Db, f: impl FnOnce(&mut LibraryQuery)) -> Vec<String> {
    let mut query = LibraryQuery::default();
    f(&mut query);
    let t = titles(db, &query);
    assert_eq!(db.library_count(&query).unwrap(), t.len());
    t
}

#[test]
fn filter_by_status() {
    let (db, _) = filter_fixture();
    assert_eq!(filtered(&db, |_| {}).len(), 9);
    assert_eq!(
        filtered(&db, |q| q.status = Some(Status::Done)),
        vec!["Cooking", "Rust advanced", "Rust intro"]
    );
    assert_eq!(
        filtered(&db, |q| q.status = Some(Status::Failed)),
        vec!["Cooking failed", "Rust failed"]
    );
    assert_eq!(
        filtered(&db, |q| q.status = Some(Status::Skipped)),
        vec!["Skipped rust"]
    );
    assert_eq!(
        filtered(&db, |q| q.status = Some(Status::Running)),
        vec!["Running rust"]
    );
    assert_eq!(
        filtered(&db, |q| q.status = Some(Status::Paused)),
        vec!["Paused"]
    );
    // Queued rows are never library rows, even when asked for explicitly.
    assert!(filtered(&db, |q| q.status = Some(Status::Queued)).is_empty());
    for s in Status::ALL {
        for r in all_rows(
            &db,
            &LibraryQuery {
                status: Some(s),
                ..Default::default()
            },
        ) {
            assert_eq!(r.status, Some(s));
        }
    }
}

#[test]
fn filter_missing_only() {
    let (mut db, ids) = filter_fixture();
    assert_eq!(
        filtered(&db, |q| q.missing_only = true),
        vec!["Cooking failed", "Rust intro"]
    );
    let rows = all_rows(
        &db,
        &LibraryQuery {
            missing_only: true,
            ..Default::default()
        },
    );
    assert!(rows.iter().all(|r| r.missing));
    // set_file clears the flag again.
    db.set_file(ids[0], "C:/found.mp4", Some(1)).unwrap();
    assert_eq!(
        filtered(&db, |q| q.missing_only = true),
        vec!["Cooking failed"]
    );
    db.set_missing(&[(ids[4], false)]).unwrap();
    assert!(filtered(&db, |q| q.missing_only = true).is_empty());
}

#[test]
fn filter_channel_is_case_insensitive_and_exact() {
    let (db, _) = filter_fixture();
    let rust_channel = vec![
        "Cancelled",
        "Running rust",
        "Rust advanced",
        "Rust failed",
        "Rust intro",
    ];
    for c in [
        "Rust Channel",
        "rust channel",
        "RUST CHANNEL",
        "rUsT cHaNnEl",
    ] {
        assert_eq!(
            filtered(&db, |q| q.channel = c.into()),
            rust_channel,
            "{c:?}"
        );
    }
    assert_eq!(filtered(&db, |q| q.channel = "kitchen".into()).len(), 4);
    // Exact match, not prefix or substring.
    assert!(filtered(&db, |q| q.channel = "Rust".into()).is_empty());
    assert!(filtered(&db, |q| q.channel = "Rust Channel ".into()).is_empty());
    assert!(filtered(&db, |q| q.channel = "%".into()).is_empty());
}

#[test]
fn filter_combinations_with_search() {
    let (db, _) = filter_fixture();
    assert_eq!(
        filtered(&db, |q| q.text = "rust".into()),
        vec![
            "Cancelled",
            "Running rust",
            "Rust advanced",
            "Rust failed",
            "Rust intro",
            "Skipped rust"
        ]
    );
    assert_eq!(
        filtered(&db, |q| {
            q.text = "rust".into();
            q.status = Some(Status::Done);
        }),
        vec!["Rust advanced", "Rust intro"]
    );
    assert_eq!(
        filtered(&db, |q| {
            q.text = "rust".into();
            q.channel = "KITCHEN".into();
        }),
        vec!["Skipped rust"]
    );
    assert_eq!(
        filtered(&db, |q| {
            q.text = "cook".into();
            q.missing_only = true;
        }),
        vec!["Cooking failed"]
    );
    assert_eq!(
        filtered(&db, |q| {
            q.text = "rust".into();
            q.status = Some(Status::Done);
            q.missing_only = true;
            q.channel = "rust channel".into();
        }),
        vec!["Rust intro"]
    );
    assert!(
        filtered(&db, |q| {
            q.text = "cook".into();
            q.channel = "rust channel".into();
        })
        .is_empty()
    );
    // Search matches channel text too, then the status filter narrows it.
    assert_eq!(
        filtered(&db, |q| {
            q.text = "kitchen".into();
            q.status = Some(Status::Failed);
        }),
        vec!["Cooking failed"]
    );
}

// ---------------------------------------------------------------------------------------------
// Paging
// ---------------------------------------------------------------------------------------------

fn assert_paging(db: &Db, query: &LibraryQuery, page: usize) {
    let total = db.library_count(query).unwrap();
    let full = ids(&all_rows(db, query));
    assert_eq!(full.len(), total);
    let mut seen = Vec::new();
    let mut offset = 0;
    loop {
        let p = db.library_page(query, offset, page).unwrap();
        if offset < total {
            assert_eq!(p.len(), page.min(total - offset), "offset {offset}");
        } else {
            assert!(p.is_empty());
            break;
        }
        assert!(!p.is_empty());
        seen.extend(ids(&p));
        offset += page;
    }
    assert_eq!(
        seen, full,
        "pages must concatenate to the full ordered list"
    );
    let mut dedup = seen.clone();
    dedup.sort_unstable();
    dedup.dedup();
    assert_eq!(dedup.len(), total, "pages must not overlap");
}

#[test]
fn paging_is_contiguous_and_non_overlapping() {
    let db = Db::open_memory().unwrap();
    for i in 0..53 {
        let mut d = dl(
            &format!(
                "Video {} {}",
                i % 7,
                if i % 2 == 0 { "even" } else { "odd" }
            ),
            ["Alpha", "beta", "GAMMA"][i % 3],
            if i % 5 == 0 {
                Status::Failed
            } else {
                Status::Done
            },
        );
        d.created_at = 1000 + (i as i64 % 4); // many ties
        d.duration = (i % 3 != 0).then_some((i % 6) as f64);
        let id = db.insert_download(&d).unwrap();
        db.set_file(id, "x.mp4", (i % 4 != 0).then_some((i % 5) as i64))
            .unwrap();
    }
    add(&db, "Video queued", "Alpha", Status::Queued);
    assert_eq!(count(&db, ""), 53);

    // 53 rows in pages of 10: five full pages and a short last page of 3.
    let query = LibraryQuery::default();
    let pages: Vec<usize> = (0..6)
        .map(|p| db.library_page(&query, p * 10, 10).unwrap().len())
        .collect();
    assert_eq!(pages, vec![10, 10, 10, 10, 10, 3]);
    assert!(db.library_page(&query, 60, 10).unwrap().is_empty());
    assert!(db.library_page(&query, 10_000, 10).unwrap().is_empty());
    assert!(db.library_page(&query, 0, 0).unwrap().is_empty());

    for sort in ALL_SORTS {
        for descending in [false, true] {
            for page in [1, 7, 10, 53, 100] {
                let query = LibraryQuery {
                    sort,
                    descending,
                    ..Default::default()
                };
                assert_paging(&db, &query, page);
                let query = LibraryQuery {
                    text: "even".into(),
                    sort,
                    descending,
                    ..Default::default()
                };
                assert_paging(&db, &query, page);
                let query = LibraryQuery {
                    status: Some(Status::Done),
                    channel: "gamma".into(),
                    sort,
                    descending,
                    ..Default::default()
                };
                assert_paging(&db, &query, page);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// FTS sync on updates and deletes
// ---------------------------------------------------------------------------------------------

#[test]
fn update_meta_keeps_fts_in_sync() {
    let mut db = Db::open_memory().unwrap();
    let id = add(&db, "Original title", "First channel", Status::Done);
    let other = add(&db, "Unrelated", "Other", Status::Done);
    assert_eq!(count(&db, "original"), 1);

    let mut d = db.get_download(id).unwrap().unwrap();
    d.title = "Renamed video".into();
    d.channel = "Second channel".into();
    d.description = "Fresh description text".into();
    d.duration = Some(12.5);
    d.upload_date = "20260101".into();
    db.update_meta(id, &d).unwrap();

    for gone in ["original", "first"] {
        assert_eq!(count(&db, gone), 0, "{gone}");
    }
    for found in ["renamed", "second", "fresh", "description"] {
        assert_eq!(ids(&all_rows(&db, &q(found))), vec![id], "{found}");
    }
    assert_eq!(count(&db, "channel"), 1);
    let got = db.get_download(id).unwrap().unwrap();
    assert_eq!(got.title, "Renamed video");
    assert_eq!(got.channel, "Second channel");
    assert_eq!(got.description, "Fresh description text");
    assert_eq!(got.duration, Some(12.5));
    assert_eq!(got.upload_date, "20260101");

    // Rewriting the same meta must not duplicate index entries.
    for _ in 0..3 {
        db.update_meta(id, &d).unwrap();
    }
    assert_eq!(fts_hits(&db, "\"renamed\"*"), 1);

    // Updates to non-indexed columns leave search alone.
    db.set_file(id, "C:/x.mp4", Some(42)).unwrap();
    db.set_status(id, Status::Failed, "boom").unwrap();
    db.set_missing(&[(id, true)]).unwrap();
    assert_eq!(ids(&all_rows(&db, &q("renamed"))), vec![id]);

    // Clearing the description removes its words.
    d.description.clear();
    db.update_meta(id, &d).unwrap();
    assert_eq!(count(&db, "fresh"), 0);
    assert_eq!(ids(&all_rows(&db, &q("unrelated"))), vec![other]);
    fts_integrity(&db);
}

#[test]
fn update_meta_on_unknown_id_is_a_no_op() {
    let db = Db::open_memory().unwrap();
    let id = add(&db, "Kept", "Chan", Status::Done);
    db.update_meta(id + 100, &dl("Ghost", "Ghost", Status::Done))
        .unwrap();
    assert_eq!(count(&db, "ghost"), 0);
    assert_eq!(count(&db, "kept"), 1);
    assert!(db.get_download(id + 100).unwrap().is_none());
    fts_integrity(&db);
}

#[test]
fn delete_downloads_many_ids_keeps_fts_in_sync() {
    let mut db = Db::open_memory().unwrap();
    let batch: Vec<Download> = (0..2500)
        .map(|i| {
            dl(
                &format!("{} clip {i}", if i % 2 == 0 { "keeper" } else { "doomed" }),
                if i % 3 == 0 { "Trio" } else { "Duo" },
                Status::Done,
            )
        })
        .collect();
    let all = db.insert_downloads(&batch).unwrap();
    assert_eq!(all.len(), 2500);
    assert!(all.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(count(&db, "doomed"), 1250);
    assert_eq!(count(&db, "keeper"), 1250);

    let doomed: Vec<i64> = all.iter().copied().skip(1).step_by(2).collect();
    // Duplicates and unknown ids in the list are tolerated.
    let mut to_delete = doomed.clone();
    to_delete.extend_from_slice(&doomed[..10]);
    to_delete.push(999_999);
    db.delete_downloads(&to_delete).unwrap();

    assert_eq!(count(&db, ""), 1250);
    assert_eq!(count(&db, "doomed"), 0);
    assert_eq!(fts_hits(&db, "\"doomed\"*"), 0);
    assert_eq!(count(&db, "keeper"), 1250);
    assert_eq!(count(&db, "trio"), 417);
    for id in doomed.iter().take(20) {
        assert!(db.get_download(*id).unwrap().is_none());
    }
    fts_integrity(&db);

    db.delete_downloads(&[]).unwrap();
    db.delete_downloads(&all).unwrap();
    assert_eq!(count(&db, ""), 0);
    assert_eq!(fts_hits(&db, "\"keeper\"*"), 0);
    fts_integrity(&db);
}

#[test]
fn fts_survives_reopen_on_disk() {
    let dir = TempDir::new("reopen");
    let path = dir.path.join("lib.db");
    let id = {
        let db = Db::open(&path).unwrap();
        let id = add(&db, "Persistent café", "Disk", Status::Done);
        add(&db, "Volatile", "Disk", Status::Done);
        id
    };
    {
        // Re-running the schema on an existing file must be harmless.
        let mut db = Db::open(&path).unwrap();
        assert_eq!(count(&db, ""), 2);
        assert_eq!(ids(&all_rows(&db, &q("cafe"))), vec![id]);
        let other: Vec<i64> = all_rows(&db, &q("volatile")).iter().map(|r| r.id).collect();
        db.delete_downloads(&other).unwrap();
        fts_integrity(&db);
    }
    let db = Db::open(&path).unwrap();
    assert_eq!(count(&db, ""), 1);
    assert_eq!(count(&db, "volatile"), 0);
    assert_eq!(count(&db, "persist disk"), 1);
}

// ---------------------------------------------------------------------------------------------
// Records, files and queue interplay
// ---------------------------------------------------------------------------------------------

#[test]
fn get_download_and_files() {
    let mut db = Db::open_memory().unwrap();
    let mut d = dl("Full record", "Chan", Status::Done);
    d.video_id = "abc123".into();
    d.extractor = "youtube".into();
    d.duration = Some(61.25);
    d.thumbnail_url = "https://i/x.jpg".into();
    d.preset = "Best".into();
    d.options_json = "{\"a\":1}".into();
    d.created_at = 1_700_000_000;
    let a = db.insert_download(&d).unwrap();
    let got = db.get_download(a).unwrap().unwrap();
    assert_eq!(got.id, a);
    assert_eq!(got.video_id, "abc123");
    assert_eq!(got.extractor, "youtube");
    assert_eq!(got.url, d.url);
    assert_eq!(got.title, "Full record");
    assert_eq!(got.duration, Some(61.25));
    assert_eq!(got.thumbnail_url, "https://i/x.jpg");
    assert_eq!(got.preset, "Best");
    assert_eq!(got.options_json, "{\"a\":1}");
    assert_eq!(got.status, Some(Status::Done));
    assert_eq!(got.created_at, 1_700_000_000);
    assert!(got.in_queue);
    assert!(!got.missing);
    assert_eq!(got.file_size, None);
    assert!(db.get_download(a + 1).unwrap().is_none());

    let b = add(&db, "Failed with file", "Chan", Status::Failed);
    let c = add(&db, "Done without file", "Chan", Status::Done);
    let e = add(&db, "Done with file", "Chan", Status::Done);
    db.set_file(a, "C:/a.mp4", Some(10)).unwrap();
    db.set_file(b, "C:/b.mp4", Some(20)).unwrap();
    db.set_file(e, "C:/e.mp4", None).unwrap();
    let _ = c;
    db.set_missing(&[(e, true)]).unwrap();

    // `..` keeps this independent of any extra columns `files()` returns.
    let mut files: Vec<(i64, String, bool)> = db
        .files()
        .unwrap()
        .into_iter()
        .map(|(id, path, missing, ..)| (id, path, missing))
        .collect();
    files.sort();
    assert_eq!(
        files,
        vec![
            (a, "C:/a.mp4".to_owned(), false),
            (e, "C:/e.mp4".to_owned(), true)
        ]
    );
    let got = db.get_download(e).unwrap().unwrap();
    assert!(got.missing);
    assert_eq!(got.file_size, None);
    assert_eq!(db.get_download(a).unwrap().unwrap().file_size, Some(10));
}

#[test]
fn queue_and_library_views() {
    let mut db = Db::open_memory().unwrap();
    let a = add(&db, "Alpha", "C", Status::Queued);
    let b = add(&db, "Bravo", "C", Status::Done);
    let c = add(&db, "Charlie", "C", Status::Skipped);
    let d = add(&db, "Delta", "C", Status::Failed);
    let batch = db
        .insert_downloads(&[
            dl("Echo", "C", Status::Queued),
            dl("Foxtrot", "C", Status::Queued),
        ])
        .unwrap();
    let (e, f) = (batch[0], batch[1]);

    let queue_ids = |db: &Db| db.queue().unwrap().iter().map(|d| d.id).collect::<Vec<_>>();
    assert_eq!(queue_ids(&db), vec![a, b, c, d, e, f]);
    let pos: Vec<f64> = db.queue().unwrap().iter().map(|d| d.queue_pos).collect();
    assert_eq!(pos, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);

    // Library is everything that is not queued, regardless of queue membership.
    assert_eq!(
        titles(&db, &LibraryQuery::default()),
        vec!["Bravo", "Charlie", "Delta"]
    );

    // The engine always passes the whole queue.
    db.reorder_queue(&[f, d, a, b, c, e]).unwrap();
    assert_eq!(queue_ids(&db), vec![f, d, a, b, c, e]);

    // Move b between f (1) and d (2).
    db.set_queue_pos(b, 1.5).unwrap();
    assert_eq!(&queue_ids(&db)[..4], &[f, b, d, a]);
    // Equal positions fall back to id order.
    db.set_queue_pos(e, 1.5).unwrap();
    assert_eq!(&queue_ids(&db)[..5], &[f, b, e, d, a]);

    // Only done and skipped leave the queue; they stay in the library.
    assert_eq!(db.clear_finished().unwrap(), 2);
    assert_eq!(db.clear_finished().unwrap(), 0);
    let q = queue_ids(&db);
    assert!(!q.contains(&b) && !q.contains(&c));
    assert_eq!(q.len(), 4);
    assert!(!db.get_download(b).unwrap().unwrap().in_queue);
    assert_eq!(
        titles(&db, &LibraryQuery::default()),
        vec!["Bravo", "Charlie", "Delta"]
    );

    // New entries go to the back of the remaining queue.
    let g = add(&db, "Golf", "C", Status::Queued);
    assert_eq!(*queue_ids(&db).last().unwrap(), g);
    let h = db
        .insert_downloads(&[dl("Hotel", "C", Status::Queued)])
        .unwrap()[0];
    assert_eq!(*queue_ids(&db).last().unwrap(), h);
    let pos: Vec<f64> = db.queue().unwrap().iter().map(|d| d.queue_pos).collect();
    assert!(pos.windows(2).all(|w| w[0] <= w[1]), "{pos:?}");

    db.delete_downloads(&[a, g]).unwrap();
    assert!(!queue_ids(&db).contains(&a));
    assert!(!queue_ids(&db).contains(&g));
}

// ---------------------------------------------------------------------------------------------
// Performance
// ---------------------------------------------------------------------------------------------

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("beardl-test-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn timed<T>(label: &str, f: impl FnOnce() -> T) -> (T, Duration) {
    let start = Instant::now();
    let out = f();
    let took = start.elapsed();
    eprintln!("{label:<44} {:>8.2} ms", took.as_secs_f64() * 1000.0);
    (out, took)
}

#[test]
#[ignore = "performance; run with --release -- --ignored --nocapture"]
fn library_100k_performance() {
    const N: usize = 100_000;
    let limit = if cfg!(debug_assertions) {
        Duration::from_millis(500)
    } else {
        Duration::from_millis(50)
    };
    let dir = TempDir::new("perf");
    let path = dir.path.join("library.db");
    {
        let mut db = Db::open(&path).unwrap();

        let words = [
            "rust",
            "tutorial",
            "cooking",
            "pasta",
            "travel",
            "vlog",
            "music",
            "live",
            "review",
            "unboxing",
            "café",
            "history",
            "science",
            "space",
            "gaming",
            "speedrun",
            "podcast",
            "interview",
            "news",
            "documentary",
        ];
        let channels = [
            "Alpha",
            "beta",
            "Gamma Ray",
            "Delta Force",
            "epsilon",
            "Zeta",
            "Eta Carinae",
            "Theta",
            "Iota",
            "Kappa",
        ];
        let start = Instant::now();
        let rows: Vec<Download> = (0..N)
            .map(|i| {
                let w = |k: usize| words[(i / k + k * 7) % words.len()];
                Download {
                    url: format!("https://example.com/watch?v={i}"),
                    video_id: format!("v{i:08}"),
                    title: format!("{} {} {} #{i}", w(1), w(3), w(11)),
                    channel: format!("{} {}", channels[i % channels.len()], i % 1000),
                    duration: (i % 13 != 0).then_some(((i * 37) % 10_000) as f64 + 0.5),
                    status: Some(Status::Queued),
                    created_at: 1_600_000_000 + (i as i64 * 61) % 50_000_000,
                    ..Default::default()
                }
            })
            .collect();
        let built = start.elapsed();
        db.insert_downloads(&rows).unwrap();
        let inserted = start.elapsed();
        {
            let tx = db.conn.transaction().unwrap();
            tx.execute(
                "UPDATE downloads SET status = 'done', in_queue = 0, \
                 finished_at = created_at + 600, \
                 file_path = 'C:/Videos/' || video_id || '.mp4', \
                 file_size = CASE WHEN id % 17 = 0 THEN NULL \
                                  ELSE (id * 2654435761) % 4000000000 END",
                [],
            )
            .unwrap();
            tx.commit().unwrap();
        }
        eprintln!(
            "setup: {N} rows: build {:.0} ms, insert_downloads {:.0} ms, status UPDATE {:.0} ms",
            built.as_secs_f64() * 1000.0,
            (inserted - built).as_secs_f64() * 1000.0,
            (start.elapsed() - inserted).as_secs_f64() * 1000.0
        );

        let all = LibraryQuery::default();
        let (n, t1) = timed("library_count (no filter)", || {
            db.library_count(&all).unwrap()
        });
        assert_eq!(n, N);

        let search = q("rust");
        let (hits, t2) = timed("library_count (search \"rust\")", || {
            db.library_count(&search).unwrap()
        });
        assert!(hits > 0 && hits < N, "{hits}");

        let (p0, t3) = timed("library_page offset 0 (Date desc)", || {
            db.library_page(
                &LibraryQuery {
                    descending: true,
                    ..Default::default()
                },
                0,
                100,
            )
            .unwrap()
        });
        assert_eq!(p0.len(), 100);

        let by_title = LibraryQuery {
            sort: SortKey::Title,
            ..Default::default()
        };
        let (deep, t4) = timed("library_page offset 99,900 (Title asc)", || {
            db.library_page(&by_title, 99_900, 100).unwrap()
        });
        assert_eq!(deep.len(), 100);
        assert!(
            deep.windows(2)
                .all(|w| w[0].title.to_ascii_lowercase() <= w[1].title.to_ascii_lowercase())
        );

        let by_size = LibraryQuery {
            sort: SortKey::Size,
            descending: true,
            ..Default::default()
        };
        let (big, t5) = timed("library_page offset 0 (Size desc)", || {
            db.library_page(&by_size, 0, 100).unwrap()
        });
        assert_eq!(big.len(), 100);
        assert!(big.iter().all(|r| r.file_size.is_some()));
        assert!(
            big.windows(2)
                .all(|w| w[0].file_size.cmp(&w[1].file_size) != Ordering::Less)
        );

        let limit_ms = limit.as_millis();
        for (label, t) in [
            ("library_count (no filter)", t1),
            ("library_count (search)", t2),
            ("library_page offset 0", t3),
            ("library_page offset 99,900 by Title", t4),
            ("library_page by Size desc", t5),
        ] {
            assert!(
                t < limit,
                "{label} took {:.2} ms, limit {limit_ms} ms",
                t.as_secs_f64() * 1000.0
            );
        }
    }
    // `dir` drops here, after the Db is closed, removing the db, -wal and -shm files.
}

/// Writes a 100,000-entry library to `BEARDL_GEN_DB` for manual UI testing.
#[test]
#[ignore]
fn generate_100k_library() {
    let Some(path) = std::env::var_os("BEARDL_GEN_DB") else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    let _ = std::fs::remove_file(&path);
    let mut db = Db::open(&path).unwrap();
    let words = [
        "rust", "cooking", "travel", "music", "review", "science", "history", "space",
    ];
    let rows: Vec<Download> = (0..100_000)
        .map(|i| Download {
            url: format!("https://example.com/watch?v={i}"),
            video_id: format!("v{i:08}"),
            title: format!(
                "{} {} episode {i}",
                words[i % words.len()],
                words[(i / 7) % words.len()]
            ),
            channel: format!("Channel {}", i % 500),
            duration: Some((i % 7200) as f64 + 30.0),
            status: Some(Status::Queued),
            created_at: 1_600_000_000 + i as i64 * 300,
            ..Default::default()
        })
        .collect();
    db.insert_downloads(&rows).unwrap();
    db.conn
        .execute(
            "UPDATE downloads SET status = 'done', in_queue = 0, file_size = id * 1000003 % 4000000000, \
             file_path = 'C:/Videos/' || video_id || '.mp4'",
            [],
        )
        .unwrap();
}
