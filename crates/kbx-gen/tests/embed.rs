//! The real model code on the tiny fixed model, and the download.

use std::path::Path;
use std::thread;

use kbx_gen::{BertEmbedder, Embedder, MODEL, download};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tiny_http::{Response, Server};

const TINY: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/tiny-model"
);

fn tiny() -> BertEmbedder {
    BertEmbedder::load(&MODEL, Path::new(TINY)).unwrap()
}

fn length(vector: &[f32]) -> f32 {
    vector.iter().map(|x| x * x).sum::<f32>().sqrt()
}

#[test]
fn vectors_have_unit_length_and_repeat() {
    let model = tiny();
    let first = model
        .embed(&["token buckets", "a much longer text about a kitchen"])
        .unwrap();
    assert_eq!(first.len(), 2);
    assert_eq!(first[0].len(), 16);
    assert!((length(&first[0]) - 1.0).abs() < 1e-4);
    assert_ne!(first[0], first[1]);

    let alone = model.embed(&["token buckets"]).unwrap();
    let close = alone[0]
        .iter()
        .zip(&first[0])
        .all(|(a, b)| (a - b).abs() < 1e-4);
    assert!(close, "padding changed a vector");
}

#[test]
fn text_longer_than_the_model_reads_is_cut_not_refused() {
    let long = "word ".repeat(20_000);
    let vectors = tiny().embed(&[&long, "é".repeat(30_000).as_str()]).unwrap();
    assert_eq!(vectors.len(), 2);
}

#[test]
fn more_texts_than_a_batch_come_back_in_order() {
    let texts: Vec<String> = (0..40).map(|n| format!("note {n}")).collect();
    let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
    let model = tiny();
    let all = model.embed(&texts).unwrap();
    assert_eq!(all.len(), 40);
    let last = model.embed(&texts[39..]).unwrap();
    assert!(
        all[39]
            .iter()
            .zip(&last[0])
            .all(|(a, b)| (a - b).abs() < 1e-4)
    );
}

#[test]
fn files_that_are_not_a_model_are_an_error() {
    let dir = TempDir::new().unwrap();
    assert!(BertEmbedder::load(&MODEL, dir.path()).is_err());
}

fn serve(body: &'static str) -> String {
    let server = Server::http("127.0.0.1:0").unwrap();
    let port = server.server_addr().to_ip().unwrap().port();
    thread::spawn(move || {
        for request in server.incoming_requests() {
            let _ = request.respond(Response::from_string(body));
        }
    });
    format!("http://127.0.0.1:{port}/file")
}

fn sha256(text: &str) -> String {
    let digest = Sha256::digest(text);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn a_download_with_the_right_digest_arrives() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("file");
    download(&serve("weights"), &sha256("weights"), &path).unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), "weights");
}

#[test]
fn a_download_with_the_wrong_digest_leaves_nothing() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("file");
    let error = download(&serve("tampered"), &sha256("weights"), &path).unwrap_err();
    assert!(error.to_string().contains("SHA-256"), "{error}");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}
