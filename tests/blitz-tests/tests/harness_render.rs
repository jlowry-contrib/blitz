//! Rendering a harness document to pixels.

use anyrender::ImageRenderer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_test_harness::{Harness, HarnessOptions};

#[test]
fn harness_renders_pixels() {
    let mut harness = Harness::from_html_with(
        r#"<html><body style="margin:0">
            <div style="width:20px; height:10px; background:rgb(255, 0, 0)"></div>
        </body></html>"#,
        HarnessOptions {
            width: 20,
            height: 10,
            ..Default::default()
        },
    );
    let mut renderer = VelloCpuImageRenderer::new(1, 1);

    let pixels = harness.render(&mut renderer);

    assert_eq!(pixels.len(), 800);
    assert!(
        pixels
            .as_chunks::<4>()
            .0
            .iter()
            .all(|px| *px == [255, 0, 0, 255]),
        "{pixels:?}"
    );
}
