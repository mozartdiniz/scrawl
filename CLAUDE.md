# Scrawl — Project Brief for Claude

## Vision

macOS Preview's Markup, on Linux: open an image (or paste a screenshot),
annotate it with arrows, shapes, text, highlights and redactions, crop, then
save or copy. Native GNOME app; same stack as the Amadeuz Linux client.

Design is based on a reverse-engineering study of Preview.app; see
`PREVIEW_RESEARCH.md` before changing the architecture.

## Stack

- Rust 2021, `gtk4` 0.10 (`gnome_46`), `libadwaita` 0.8 (`v1_5`)
- Cairo for drawing, `pangocairo` for text, `gdk-pixbuf` for loading and JPEG
- No Meson/Blueprint: UI is built in Rust, icons are compiled into a
  GResource by `build.rs`, so `cargo run` works with no install step
  (on macOS too, with Homebrew's gtk4 and libadwaita)

## Layout

```
src/
  main.rs     app, accelerators
  model.rs    Point/Rect/Color/Style/Shape/Annotation, in image-pixel coords
  editor.rs   controller (AKController equivalent): tools, selection, drag,
              handles, hit testing, undo, resize. No widgets → unit-tested
  sketch.rs   Sketch tool shape recognition (line / rectangle / oval)
  render.rs   cairo renderer; used both for the canvas and for flattening
  imageio.rs  pixbuf/texture ↔ cairo surface, save PNG/JPEG/WebP
  window.rs   host: header bar, markup toolbar, canvas, gestures, dialogs
  resize_dialog.rs  Adjust Size (Preview's Image Dimensions sheet)
data/         desktop file, metainfo, app icon, symbolic tool icons
```

## Key decisions

- **Live model, flatten on save**, like Preview. Crop is a rect in the
  model. The base image only changes on Adjust Size (resample), which
  scales annotations along with it; undo snapshots hold the surface.
- **Resample off = pixels locked**, only the resolution changes (Preview
  asserts the same). The resolution is read from and written to PNG/JPEG.
- **Ctrl+S never silently overwrites the source image.** The first save
  always asks for a destination (default `<name>-annotated.png`); later
  saves go there.
- The window owns its `Inner` state through a strong ref captured in a
  `destroy` handler. Every other closure holds a `Weak`, and `Inner` holds
  the window weakly, so there is no reference cycle.
- Per-tool styles: changing a style with nothing selected changes that
  tool's default. Restyling a selection also updates its tool's default.

## Commands

`cargo run -- [image]`, `cargo test`, `just install`, `just flatpak`.
Visual check of the renderer:
`SCRAWL_RENDER_OUT=/tmp/out.png cargo test flatten`.
