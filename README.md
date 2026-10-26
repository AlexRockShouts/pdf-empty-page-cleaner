# PDF Empty Page Cleaner

A small offline desktop app for **Windows, macOS and Linux** that removes empty pages from PDF files.

- Drag & drop one or many PDF files (or click to choose files).
- A page is treated as **empty when it draws no text and no images**.
- The cleaned file is saved next to the original as `<name>-cleaned.pdf`.
- Everything runs locally. Nothing is uploaded anywhere.
- Single executable. No Python, no installer, no runtime to install.

## Download

Grab the file for your system from the [latest release](../../releases/latest):

| System | File | Notes |
| --- | --- | --- |
| Windows 10/11 (x64) | `PDFCleaner-windows-x64.exe` | Single `.exe`. Windows SmartScreen may warn because the app is not code-signed: choose **More info -> Run anyway**. |
| macOS (Intel + Apple Silicon) | `PDFCleaner-macos-universal.zip` | Unzip and open `PDFCleaner.app`. The app is unsigned, so the first launch may be blocked: right-click the app, choose **Open**, then confirm. Alternatively run `xattr -dr com.apple.quarantine /path/to/PDFCleaner.app`. |
| Linux (x64) | `PDFCleaner-linux-x64.tar.gz` | Extract, then `chmod +x PDFCleaner-linux-x64` and run it. Needs a normal desktop with OpenGL (standard on mainstream distributions). |

## Usage

1. Drag PDF files onto the drop area, or click **Choose PDF files...**.
2. The app scans each page and removes the empty ones.
3. The result is written next to the original file. The window shows how many pages were removed and the new file name.

Options:

- **Also remove pages that contain only vector graphics** - by default, a page that draws lines/shapes is kept. Enable this to treat those pages as empty too.
- Pages that are entirely blank are always removed, including pages with no content stream at all.
- If every page in a file is empty, the app refuses to write an empty PDF and reports it instead.

### What counts as empty

The scan looks at the actual page content, including content streams, recursively nested form XObjects, inline images, image XObjects and annotation appearance streams:

- Visible text via `Tj`, `TJ`, `'` and `"` counts as content.
- Invisible text (render mode 3 or 7, sometimes used for OCR layers) does not count.
- Images, including images inside nested forms, count as content.
- Vector drawing (fills, strokes, shadings) counts as content unless the option above is enabled.

This means a **scanned blank page is kept**, because it is an image of a blank page. The app does not do OCR or image analysis; it uses the PDF structure.

The original file is never modified. If `<name>-cleaned.pdf` already exists, it is replaced. Password-protected PDFs are skipped (a PDF with an empty user password is supported).

## Command line (macOS and Linux)

The same binary also works without the GUI:

```bash
./pdf-cleaner file.pdf other.pdf          # clean one or more files
./pdf-cleaner --remove-graphics file.pdf  # also drop vector-only pages
./pdf-cleaner --help
```

## Build from source

Requires a recent stable [Rust](https://rustup.rs) toolchain.

```bash
cargo test
cargo build --release
# binary: target/release/pdf-cleaner
```

Platform packaging scripts write into `dist/`:

```bash
bash scripts/build-macos.sh       # universal .app + zip (macOS)
bash scripts/build-linux.sh       # tar.gz (Linux)
pwsh scripts/build-windows.ps1    # .exe (Windows)
bash scripts/build-cross.sh       # cross-build Linux + Windows from macOS/Linux
```

`build-cross.sh` needs `cargo-zigbuild` and `zig` (for example `pip install ziglang cargo-zigbuild`). The Windows executable is built for the GUI subsystem, so it opens without a console window.

The application icon is generated from `assets/AppIcon.png` with `scripts/make-icon.py`; the checked-in `.icns`/`.ico` files are already in `assets/`.

## Project layout

```
src/lib.rs        PDF scanning and page removal (no GUI dependencies)
src/gui.rs        egui/eframe desktop UI with drag & drop
src/main.rs       GUI entry point and CLI
tests/            unit and fixture tests
scripts/          build and packaging helpers
```

## Privacy

No network access, no telemetry, no temporary uploads. PDFs are read and written locally.

## License

MIT - see [LICENSE](LICENSE).
