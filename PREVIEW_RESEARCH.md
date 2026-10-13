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
| Rotate Left / Right (⌘L / ⌘R) | ✅ Ctrl+L / Ctrl+R; annotations turn with the image (§5) |
| Zoom In / Out, Actual Size, Zoom to Fit | ✅ header zoom control, Ctrl+± / Ctrl+1 / Ctrl+0, Ctrl+scroll and pinch around the pointer |
| Flip Horizontal / Vertical | ⏳ |
| Color, fill, width, dashed, shadow | ✅ |
| Select, move, resize handles, Shift constrains | ✅ |
| Front/back, duplicate, delete, nudge | ✅ |
| Undo/redo | ✅ snapshot-based |
| Copy to clipboard / new from clipboard | ✅ |
| Loupe, Mask, thought bubble, draggable bubble tail, star points/sides | ⏳ next |
| Signatures | ⏳ |
| PDF markup | ⏳ (would need poppler; much bigger scope) |
| Editable after reopening | ✳ Preview doesn't do this for images either; a sidecar file could |

## 5. Rotate Left / Right with annotations on the image

Decompiled from the macOS `AnnotationKit` image in the dyld shared cache
(`ipsw dyld extract`, SHA-256 `0d3dc388…a45c`; method addresses from
`ipsw class-dump --re`; pseudocode from REA/Ghidra, evidence
`ev_7d3cbca9…`, `ev_eb625d14…`), done on 2026-10-13. Static only.

- Rotation is an **EXIF orientation change**. Every annotation class
  overrides `flattenModelExifOrientation:withModelSize:` (the base
  `AKAnnotation` raises "must be overridden by a subclass").
- `+[AKGeometryHelper _rawAffineTransformForExifOrientation:]` is a switch
  over the 8 EXIF values: 2 → scale(−1, 1), 3 → rotate(−π), 4 → scale(1, −1),
  6 → rotate(−π/2), 8 → rotate(−3π/2), and 5/7 → flip then rotate.
- `affineTransformRecenteringAboutOriginForExifOrientation:withOriginalSize:`
  concatenates that with a translation by the image size, zeroing x or y
  depending on the orientation, so the rotated image lands back in positive
  coordinates. `affineTransformFlatteningOriginalModelExif:withOriginalModelSize:`
  swaps width/height first when `exifOrientationHasReversedAxes:`.
- Per class: `AKRectangularShapeAnnotation`, `AKDoodleAnnotation` and
  `AKCropAnnotation` set `rectangle = CGRectApplyAffineTransform(rectangle, T)`
  (bounds of the turned rect). `AKArrowAnnotation` maps its start, end and
  mid point through T by hand.
- Before that, `+adjustOriginalExifOrientationOnAnnotation:flatteningOriginalModelExif:`
  stores `concatExifOrientation(inverseExifOrientation(o), originalExifOrientation)`
  on the annotation (concat is a 9×9 lookup table). Each annotation
  remembers how far it has been turned.
- `-[AKRectangularShapeAnnotation adjustModelToCompensateForOriginalExif]`
  uses `adjustRect:forExifOrientation:aboutCenter:` with the inverse of
  that orientation: the content is laid out in the un-turned rect, then
  turned about its center. *Inference:* this is why a speech bubble's tail
  or a polygon's orientation stays put relative to the image.
- Preview's menus have Rotate Left, Rotate Right, Flip Horizontal and
  Flip Vertical, plus Actual Size, Zoom to Fit, Zoom In, Zoom Out and
  Zoom to Selection (`MainMenu.nib` strings).

**Scrawl mapping:** `Editor::rotate_image` turns the surface with an exact
quarter-turn matrix and passes the crop and every annotation through
`Point/Rect/Shape::quarter_turn` (the recentering transform). Box shapes
get `turns += 1`, which `render::outline_path` undoes by drawing in
`Rect::transposed()` and rotating about the center. Text turns about its
measured center and gains ±π/2 of its own rotation. Flips are not
implemented; they would need a mirrored `turns` (the full 8-element group).
