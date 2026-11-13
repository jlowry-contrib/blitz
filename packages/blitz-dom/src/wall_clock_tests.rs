//! Checks that blitz-dom takes all its time from the frame clock and event stamps.

use std::collections::BTreeSet;
use std::path::Path;

/// Every source file of the crate, relative to `src/`.
const SOURCES: &[(&str, &str)] = &[
    ("accessibility.rs", include_str!("accessibility.rs")),
    ("config.rs", include_str!("config.rs")),
    ("cssom.rs", include_str!("cssom.rs")),
    ("debug.rs", include_str!("debug.rs")),
    ("document.rs", include_str!("document.rs")),
    ("events/driver.rs", include_str!("events/driver.rs")),
    ("events/focus.rs", include_str!("events/focus.rs")),
    ("events/ime.rs", include_str!("events/ime.rs")),
    ("events/keyboard.rs", include_str!("events/keyboard.rs")),
    ("events/mod.rs", include_str!("events/mod.rs")),
    ("events/pointer.rs", include_str!("events/pointer.rs")),
    ("font_metrics.rs", include_str!("font_metrics.rs")),
    ("form.rs", include_str!("form.rs")),
    ("glass.rs", include_str!("glass.rs")),
    ("html.rs", include_str!("html.rs")),
    ("iframe.rs", include_str!("iframe.rs")),
    ("layout/construct.rs", include_str!("layout/construct.rs")),
    ("layout/damage.rs", include_str!("layout/damage.rs")),
    (
        "layout/inline/build.rs",
        include_str!("layout/inline/build.rs"),
    ),
    (
        "layout/inline/layout.rs",
        include_str!("layout/inline/layout.rs"),
    ),
    ("layout/inline/mod.rs", include_str!("layout/inline/mod.rs")),
    ("layout/list.rs", include_str!("layout/list.rs")),
    ("layout/mod.rs", include_str!("layout/mod.rs")),
    ("layout/paint_tree.rs", include_str!("layout/paint_tree.rs")),
    ("layout/replaced.rs", include_str!("layout/replaced.rs")),
    ("layout/table.rs", include_str!("layout/table.rs")),
    (
        "layout/text_transform.rs",
        include_str!("layout/text_transform.rs"),
    ),
    (
        "layout/writing_mode.rs",
        include_str!("layout/writing_mode.rs"),
    ),
    ("lib.rs", include_str!("lib.rs")),
    ("mutator.rs", include_str!("mutator.rs")),
    ("net.rs", include_str!("net.rs")),
    ("node/attributes.rs", include_str!("node/attributes.rs")),
    (
        "node/custom_widget.rs",
        include_str!("node/custom_widget.rs"),
    ),
    ("node/element.rs", include_str!("node/element.rs")),
    ("node/mod.rs", include_str!("node/mod.rs")),
    ("node/node.rs", include_str!("node/node.rs")),
    ("node/scrollbar.rs", include_str!("node/scrollbar.rs")),
    ("node/serialize.rs", include_str!("node/serialize.rs")),
    ("node/stylo_data.rs", include_str!("node/stylo_data.rs")),
    ("node/svg.rs", include_str!("node/svg.rs")),
    ("node/text.rs", include_str!("node/text.rs")),
    ("query_selector.rs", include_str!("query_selector.rs")),
    ("resolve.rs", include_str!("resolve.rs")),
    ("resolved_style.rs", include_str!("resolved_style.rs")),
    ("scrolling.rs", include_str!("scrolling.rs")),
    ("selection.rs", include_str!("selection.rs")),
    ("stylo.rs", include_str!("stylo.rs")),
    ("stylo_device.rs", include_str!("stylo_device.rs")),
    (
        "stylo_to_cursor_icon.rs",
        include_str!("stylo_to_cursor_icon.rs"),
    ),
    ("stylo_to_kurbo.rs", include_str!("stylo_to_kurbo.rs")),
    ("stylo_to_parley.rs", include_str!("stylo_to_parley.rs")),
    ("traversal.rs", include_str!("traversal.rs")),
    ("tree.rs", include_str!("tree.rs")),
    ("url.rs", include_str!("url.rs")),
    ("util.rs", include_str!("util.rs")),
    ("wall_clock_tests.rs", include_str!("wall_clock_tests.rs")),
];

fn list_sources(dir: &Path, prefix: &str, out: &mut BTreeSet<String>) {
    for entry in std::fs::read_dir(dir).expect("readable src directory") {
        let entry = entry.expect("readable directory entry");
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if path.is_dir() {
            list_sources(&path, &format!("{prefix}{name}/"), out);
        } else if name.ends_with(".rs") {
            out.insert(format!("{prefix}{name}"));
        }
    }
}

#[test]
fn wall_clock_scan_lists_every_file() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut on_disk = BTreeSet::new();
    list_sources(&src, "", &mut on_disk);
    let listed: BTreeSet<String> = SOURCES.iter().map(|(path, _)| path.to_string()).collect();
    assert_eq!(listed, on_disk, "SOURCES must list every file under src/");
}

#[test]
fn blitz_dom_reads_no_wall_clock() {
    // Split so that this file's own source doesn't match.
    let forbidden = [
        concat!("SystemTime", "::now"),
        concat!("Instant", "::now"),
        concat!(".elapsed", "()"),
    ];
    let hits: Vec<String> = SOURCES
        .iter()
        .flat_map(|(path, source)| {
            source
                .lines()
                .enumerate()
                .filter(move |(_, line)| forbidden.iter().any(|pattern| line.contains(pattern)))
                .map(move |(index, line)| format!("{path}:{}: {}", index + 1, line.trim()))
        })
        .collect();
    assert!(hits.is_empty(), "wall-clock reads in blitz-dom: {hits:#?}");
}
