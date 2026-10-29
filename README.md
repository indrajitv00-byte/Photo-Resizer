# Photo Resizer (Tauri)

Batch image resizer for Windows and macOS. Rust backend, web interface.

## One-time setup on Windows
1. Install **Rust**: download `rustup-init.exe` from https://rustup.rs and run it (accept the defaults).
   It asks you to install the **Visual Studio C++ Build Tools** - choose "Desktop development with C++".
2. Close and reopen PowerShell, then check: `rustc --version`
3. WebView2 is already part of Windows 10/11.

## One-time setup on macOS
`xcode-select --install`, then install Rust from https://rustup.rs

## Run
    npm install
    npx tauri icon app-icon.png     (once - creates the src-tauri/icons folder)
    npm run dev

The first run compiles everything and takes several minutes. Later runs start in seconds.

## Build an installer (run on each OS)
    npm run build
The installer appears in `src-tauri/target/release/bundle`.

## Notes
- JPG uses the Quality slider. WebP is saved lossless. PNG, TIFF, GIF and BMP ignore Quality.
- Metadata (EXIF) is not copied to the output files.
- Text watermark uses the bundled DejaVu Sans Bold font (free licence).
