use pdf_cleaner::{clean_pdf, CleanOptions};
use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn handles_a_hand_written_pdf() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mixed.pdf");
    let dir = std::env::temp_dir().join(format!(
        "pdf-cleaner-fixture-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    let source = dir.join("mixed.pdf");
    fs::copy(&fixture, &source).unwrap();

    let report = clean_pdf(&source, CleanOptions::default());
    assert!(report.error.is_none(), "{:?}", report.error);
    assert_eq!(report.total_pages, 4);
    assert_eq!(report.removed_pages, vec![1]);

    let output: PathBuf = dir.join("mixed-cleaned.pdf");
    assert_eq!(report.output.as_deref(), Some(output.as_path()));
    assert!(output.exists());

    let cleaned = std::fs::read(&output).unwrap();
    assert!(cleaned.starts_with(b"%PDF"));

    let graphics = clean_pdf(
        &source,
        CleanOptions {
            remove_graphics_only_pages: true,
        },
    );
    assert!(graphics.error.is_none());
    assert_eq!(graphics.removed_pages, vec![1, 4]);

    let _ = fs::remove_dir_all(dir);
}
