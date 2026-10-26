#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod gui;

use pdf_cleaner::{clean_pdf_guarded, CleanOptions};
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        gui::run();
    } else {
        run_cli(&args);
    }
}

fn run_cli(args: &[String]) {
    let mut options = CleanOptions::default();
    let mut files: Vec<PathBuf> = Vec::new();

    for arg in args {
        match arg.as_str() {
            "-h" | "--help" => {
                print_help();
                return;
            }
            "--remove-graphics" => options.remove_graphics_only_pages = true,
            other if other.starts_with('-') => {
                eprintln!("Unknown option: {other}\n");
                print_help();
                std::process::exit(2);
            }
            other => files.push(PathBuf::from(other)),
        }
    }

    if files.is_empty() {
        print_help();
        std::process::exit(2);
    }

    let mut failed = false;
    for file in files {
        let report = clean_pdf_guarded(&file, options);
        if let Some(error) = &report.error {
            failed = true;
            eprintln!("FAILED  {}: {error}", report.source.display());
            continue;
        }

        let output = report
            .output
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        if report.removed_pages.is_empty() {
            println!(
                "OK      {}: {} page(s), no empty pages -> {output}",
                report.source.display(),
                report.total_pages
            );
        } else {
            let pages = report
                .removed_pages
                .iter()
                .map(|page| page.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            println!(
                "OK      {}: removed {}/{} empty page(s) [{pages}] -> {output}",
                report.source.display(),
                report.removed_pages.len(),
                report.total_pages
            );
        }
        for warning in &report.warnings {
            eprintln!("WARNING {}: {warning}", report.source.display());
        }
    }

    if failed {
        std::process::exit(1);
    }
}

fn print_help() {
    println!(
        "PDF Empty Page Cleaner\n\
         \n\
         Usage:\n\
         \x20 pdf-cleaner                     open the graphical app\n\
         \x20 pdf-cleaner FILE.pdf [MORE.pdf]  clean files from the command line\n\
         \n\
         Options:\n\
         \x20 --remove-graphics    also remove pages that only contain vector graphics\n\
         \x20 -h, --help           show this help\n\
         \n\
         Cleaned files are written next to the originals as <name>-cleaned.pdf."
    );
}
