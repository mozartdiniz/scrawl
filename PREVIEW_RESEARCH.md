# How macOS Preview does Markup

Static study of `/System/Applications/Preview.app` (macOS 26, arm64 slice,
SHA-256 `b0403a45…df1ebcd`) and its private `AnnotationKit.framework`,
done with REA on 2026-10-12. Everything here comes from the shipped binary's
strings and Objective-C metadata, and from AnnotationKit's on-disk resources.
Nothing was observed at runtime. Things marked *inference* are my reading of
the evidence, not something the evidence shows directly.

## 1. Architecture: thin host, separate engine

Preview links `AnnotationKit.framework` (private) and `PaperKit.framework`
(`otool -L`).

- Preview holds an `AKController` (`_akController`, `annotationController`)
  and implements `AKControllerDelegateProtocol` /
  `PDFAKControllerDelegateProtocol` (`PVContentViewAKControllerDelegate`).
- The delegate signatures show what the **host** has to provide
  (ev_895e061a…):
  - coordinate conversion between view and model, per page:
    `{CGPoint}…Q@"AKController"` and the PaperKit logs
    `convert pointInModel` / `convert fromOverlayToModelWithPageIndex`
  - the undo manager: `@"NSUndoManager"…@"AKController"`
  - the cursor: `v…@"NSCursor"@"AKController"`
  - an overlay layer per page: `@"CALayer"…Q@"AKController"`
  - page bounds: `{CGRect}…Q@"AKController"`
- The toolbar is built from items that AnnotationKit hands over:
  `_akToolbarToolItems`, `_akToolbarAttributeItems`, `PVMarkupToolbarItem`,
  `toolbarButtonItemOfType:`.
- Newer path: there are feature flags `ImagePaperKit` and `PDFPaperKit`
  (`_isImagePaperKitEnabled`), with `PaperKitImageContainer` and
  `PaperKitWindowController` (Swift). The window controller's logs list the
  operations it handles: rotate, flip, resize, crop, autoEnhance,
  removeBackground and signature capture.

**Scrawl mapping:** `window.rs` is the host (view↔model transform, toolbar,
undo actions, cursor). `editor.rs` is the `AKController` equivalent and has
no widget dependencies.

## 2. Images: a live object model, flattened on save

- Annotations stay as objects while you edit (`flattenedAnnotations` and
  `annotationsToAuxData` are bookkeeping arrays in Preview).
- Saving renders everything into the bitmap:
  `CGImageForSavingWithFlattenedEXIF:depth:`,
  `CIImageWithFlattenedSource:edits:withFlattenScale:`,
  `_transformedCIImageFromSourceImage:editsToApply:cropToRect:withMask:withFlattenScale:`
  (ev_6d2d4d7f…).
- Crop is one of the queued *edits* (`cropToRect:`), so it stays
  non-destructive until save. EXIF orientation is applied ("flattened") at
  the same point.
- *Inference:* for raster images, saved annotations are not editable after
  reopening (PDFs keep real PDF annotations).

**Scrawl mapping:** `Document { image, annotations, crop }`. `render::flatten`
uses the same draw code as the canvas, so the export matches the screen.
Undo stores annotation and crop snapshots; the base image never changes.

## 3. Tool and attribute inventory

From AnnotationKit's `.loctable` string tables (ev_dc69409a… for the theme
plist):

| Group | Items |
|---|---|
| Shapes | Rectangle, Oval, Line, Arrow, Star, Polygon (sides/points), Speech Bubble (+tail), Thought Bubble (+tail), Heart, Triangle |
| Drawing | Doodle / Drawing (freehand), Brush Style |
| Text | Text Box, Note (popup), Text markup (highlight/underline/strikethrough, PDF) |
| Image-specific | Loupe (Magnification), Mask, Redaction, Crop, Image, Signature |
| Attributes | Color, Fill Color, Border Color, Line Width / thickness (incl. hairline), Dashed, Shadow, Corner Radius, Rotation, arrow heads at start/end/both/none, font, Multiple Fonts |
| Arrange | Bring to Front, Bring Forward, Send Backward, Send to Back, Duplicate, Cut/Copy/Paste |
| Highlight themes | yellow, green, blue, pink, purple, underline (`AKAnnotationTheme.plist`) |

Preview's highlight colors (from `AKAnnotationTheme.plist`):
yellow `#FACD5A`, green `#7CC868`, blue `#69B0F1`, pink `#FB5C89`,
purple `#C885DA`.

## 4. What Scrawl implements (v0.1)

| Preview | Scrawl |
|---|---|
| Rectangle / Oval / Line / Arrow (start, end, both) | ✅ |
| Draw (freehand) | ✅ Pen, smoothed |
| Sketch (`_akIntelligentSketchButton`) | ✅ snaps to line / rectangle / oval; "Keep Drawing" restores the stroke |
| Shapes palette: rounded rect, speech bubble, star, polygon | ✅ fixed tail / 5 points / hexagon |
| Adjust Size (Fit into, units, resolution, resample) | ✅ same rules: `cannot set widthInPixels when _resample == NO` |
| Text Box with font size and fill | ✅ |
| Rotation (text) | ✅ knob handle, Shift snaps to 15°, `[` `]` |
| Highlight | ✅ multiply-blended marker box |
| Redaction | ✅ burnt in on save |
| Crop (⌘K) | ✅ Ctrl+K, non-destructive until save |
| Color, fill, width, dashed, shadow | ✅ |
| Select, move, resize handles, Shift constrains | ✅ |
| Front/back, duplicate, delete, nudge | ✅ |
| Undo/redo | ✅ snapshot-based |
| Copy to clipboard / new from clipboard | ✅ |
| Loupe, Mask, thought bubble, draggable bubble tail, star points/sides | ⏳ next |
| Signatures | ⏳ |
| PDF markup | ⏳ (would need poppler; much bigger scope) |
| Editable after reopening | ✳ Preview doesn't do this for images either; a sidecar file could |
