# App icon

Source: `assets/icon/icon.svg` (1024×1024, macOS icon grid: body is the 824px rounded square).

Regenerate the derived files after editing the SVG (needs Node with Playwright/Chromium and ImageMagick):

    cd tools/icon && bash make.sh ../../assets/icon/icon.svg ../../assets/icon

Outputs: `icon.ico` (Windows exe, embedded by `build.rs`), `AppIcon.icns` (Mac .app bundle),
`icon-256.png` / `icon-512.png` (window, taskbar and Dock icon set at runtime in `src/main.rs`).
The Windows sizes use a tighter crop so the icon fills the small taskbar slots better.
