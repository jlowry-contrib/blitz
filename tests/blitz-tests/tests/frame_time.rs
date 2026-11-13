//! The Dioxus frame-time signal and frame requests, driven by the harness.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use blitz_dom::Document;
use blitz_test_harness::Harness;
use blitz_traits::shell::ShellProvider;
use dioxus::prelude::*;
use dioxus_core::ScopeId;
use dioxus_native::{request_animation_frame, use_frame_clock, use_frame_time};

fn frame_time_app() -> Element {
    let frame_time = use_frame_time();
    let time = frame_time();
    rsx! {
        div { id: "t", "{time.t}" }
        div { id: "dt", "{time.dt}" }
        div { id: "frame", "{time.frame}" }
    }
}

#[test]
fn frame_time_is_written_before_render() {
    let mut harness = Harness::from_component(frame_time_app);
    assert_eq!(harness.text_content("#t"), "0");

    harness.tick(0.25);
    assert_eq!(harness.text_content("#t"), "0.25");
}

#[test]
fn frame_time_counts_frames() {
    let mut harness = Harness::from_component(frame_time_app);

    harness.tick(0.5);
    harness.tick(0.5);
    harness.tick(0.25);
    assert_eq!(harness.text_content("#frame"), "3");
    assert_eq!(harness.text_content("#dt"), "0.25");
    assert_eq!(harness.text_content("#t"), "1.25");

    harness.pump();
    assert_eq!(harness.text_content("#frame"), "3");
    assert_eq!(harness.text_content("#t"), "1.25");
}

#[test]
fn request_animation_frame_is_taken_once() {
    let mut harness = Harness::from_component(frame_time_app);
    assert!(!harness.doc.take_frame_request());

    harness
        .doc
        .vdom
        .in_scope(ScopeId::ROOT, request_animation_frame);
    assert!(harness.doc.take_frame_request());
    assert!(!harness.doc.take_frame_request());
}

#[derive(Default)]
struct CountingShell(AtomicUsize);

impl ShellProvider for CountingShell {
    fn request_redraw(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn request_animation_frame_wakes_a_wall_clock_shell() {
    let mut harness = Harness::from_component(frame_time_app);
    let shell = Arc::new(CountingShell::default());
    harness
        .doc
        .vdom
        .provide_root_context(Arc::clone(&shell) as Arc<dyn ShellProvider>);

    harness
        .doc
        .vdom
        .in_scope(ScopeId::ROOT, request_animation_frame);
    assert_eq!(shell.0.load(Ordering::SeqCst), 1);
    assert!(harness.doc.take_frame_request());
}

fn frame_clock_app() -> Element {
    let clock = use_frame_clock();
    rsx! {
        div { id: "clock", "{clock.is_some()}" }
    }
}

#[test]
fn a_document_without_a_shell_has_no_frame_clock() {
    let harness = Harness::from_component(frame_clock_app);
    assert_eq!(harness.text_content("#clock"), "false");
}
