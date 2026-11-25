# Nijimi

An open-source, GPU-accelerated, headless image editing engine for the web.

[Try the playground](https://kidixdev.github.io/nijimi/) (needs a browser with WebGPU)

- **GPU-accelerated:** written in Rust, compiled to WebAssembly and rendered with WebGPU.
- **Headless:** no toolbar, panels or framework. You give it a canvas and build your own interface on its API.
- **For the web:** works in any web app or desktop webview that supports WebGPU.

The engine supports layers, groups, masks, color grading, painting, canvas and layer edits, undo and project files. The API may change.

## Layout

| Path | Purpose |
|---|---|
| `crates/core` | Document, commands, undo history, project files, strokes and viewport, with no GPU or web code |
| `crates/render` | Engine, wgpu painting and compositing, pixel readback |
| `crates/wasm` | JavaScript bindings |
| `playground` | Vite page for trying the engine |
| `xtask` | Builds the WASM package into `pkg/` |

## Commands

Requires Rust and Bun. `rust-toolchain.toml` pins Rust 1.95.0 and adds the WASM target. Update the pin in the local file and workflows together.

```bash
bun install
bun run dev        # build the engine and open the playground on :1430
bun run wasm       # rebuild the engine only
bun run test       # unit tests and GPU tests (needs a GPU adapter)
bun run lint
bun run typecheck
```

In the playground, paint with the left button, pan with the middle button and zoom with the wheel. Escape cancels a stroke. The benchmark replaces the open document.

## Usage

```ts
import init, { Editor } from 'nijimi';

await init();
const editor = await Editor.create(canvas, 1920, 1080);

editor.subscribe((event) => {
  if (event.type === 'deviceLost') restoreFromLastSave();
  else showLayers(editor.state());
});

const layer = editor.dispatch({ type: 'addLayer', name: 'Ink' });
editor.dispatch({ type: 'setLayerBlendMode', id: layer, blendMode: 'multiply' });

editor.setBrush({ diameter: 64, hardness: 0.5, flow: 0.6, opacity: 0.8, color: [0.9, 0.2, 0.4] });
editor.beginStroke(100, 100, 1);
editor.extendStroke(300, 200, 1);
editor.endStroke();
editor.render();

editor.dispatch({ type: 'undo' });

const file = await editor.saveProject();
await editor.loadProject(file);
```

The release workflow builds and publishes the tarball when a `v*` tag matches the package version. Use `bun add <tarball-url>` to install it. [GitHub Releases](https://github.com/KidiXDev/nijimi/releases)

Vite apps must add `nijimi` to `optimizeDeps.exclude`, otherwise the dev server cannot find the WASM file.

### Document and commands

- The engine owns the document. Read it with `state()` and change it with `dispatch(command)`. The `Command` type in `nijimi.d.ts` lists every command.
- `beginStroke`, `extendStroke` and `endStroke` take canvas pixels and paint on the selected layer. The stroke becomes one undoable change at `endStroke`. `cancelStroke` drops it. The `paintStroke` command paints a whole stroke in document pixels and accepts optional `dynamics`.
- `dispatch` finishes a stroke that is in progress and cancels any live preview before it runs the command.
- `dispatch` throws when a command is invalid and leaves the document alone. `addLayer`, `addGroup` and `importRgba` return the new id.
- Flow builds up within a stroke. Opacity limits the whole stroke, so it does not build up where a stroke overlaps itself.
- Groups are isolated: their layers blend with each other first, then the group blends with what is below it.
- Colors and pixels use sRGB values, and layers blend in that space.
- Events arrive after the call that caused them. On `documentReplaced`, read `state()` again.
- Undo keeps replaced tiles, up to 256 MB by default. `setHistoryLimit(bytes)` changes that. The latest change stays undoable even when it is larger than the limit, unless the limit is 0.
- `free()` releases the GPU memory.

### Editing tools

- `cropRect('wide')` returns the largest centered 16:9 rectangle that fits. Pass the result to `dispatch({ type: 'crop', rect })`. Presets include square, 4:3, 3:2, 16:9 and 9:16.
- `resizeCanvas` changes the canvas dimensions and places the old content at the optional document offset `x, y`. `extendCanvas` adds transparent margins. Crops and resizes clip pixels outside the new canvas.
- `transformLayer` applies scale, clockwise rotation in degrees, then translation. A negative scale flips an axis. The default pivot is the canvas center. Group transforms apply to all descendant raster layers. The editor resamples with nearest-neighbor sampling.
- Call `preview(command)` repeatedly with opacity or geometry changes. Each update starts from the committed state. `state()` and rendering include the preview. `commitPreview()` records one undo step and `cancelPreview()` restores the committed view. Saves capture the committed document.
- Use `Editor.brushPreset('round')` as a starting brush, `setBrushDynamics({ pressureCurve: 'soft', stabilizer: 0.5 })` to tune it, and `setEraser(true)` to erase with the same controls. Pass browser pen pressure to each stroke sample and use 1 for the mouse.
- `documentPoint(x, y)` converts canvas pixels to document coordinates. `pickColor(x, y)` reads a composited pixel as four RGBA bytes. `fill({ id, x, y, color, tolerance })` replaces the connected matching region on a raster layer. Fill uses straight RGBA values from 0 to 1, with a default tolerance of 0.
- `importRgba(width, height, pixels, { x, y, name })` adds an undoable layer at the given document position. Pixels are straight RGBA8, with transparent space where the image does not cover the canvas.
- `exportRegion(rect)` returns composite RGBA8. `exportLayer(id, rect?)` returns isolated RGBA8 and `exportLayerMask(id, rect?)` returns one alpha byte per pixel. The layer's own visibility and opacity are ignored. A group's descendants keep their appearance. Omitting the region exports the whole canvas.

### Masks and color grading

```ts
const grade = editor.dispatch({
  type: 'addAdjustment',
  adjustment: { type: 'exposure', stops: 0.5 },
})!;
editor.dispatch({ type: 'addMask', id: grade, reveal: false });
editor.beginMaskStroke(grade, 1, 100, 100, 1);
editor.extendStroke(300, 200, 1);
editor.endStroke();
editor.render();
```

- Raster layers, groups, and adjustments can each carry one mask. `addMask` defaults to revealing the layer. A mask value of 0 hides and 1 reveals. Paint with `beginMaskStroke` or the `paintMaskStroke` command. `setMaskAlpha(id, bytes)` creates or replaces the mask from one byte per canvas pixel. `exportMask(id, rect?)` reads those mask values independently, while `exportLayerMask` reads a raster layer or group's resulting alpha.
- An adjustment affects the layers beneath it in the same group or at the root. Opacity, blend mode, and the mask control its strength. `setAdjustment` replaces the settings and can also be used with `preview` for slider edits. Inspect settings and mask presence in `state()`.
- Brightness, contrast, saturation, lightness, and color balance use values from −1 to 1. HSL hue is in degrees, and exposure is in stops. Levels and curve coordinates use unit values. Curves are piecewise linear with strictly increasing x values starting at 0 and ending at 1. Blur accepts an integer radius from 0 to 32 pixels. Sharpen amount is 0 to 2. Vignette settings and grain amount use unit values. Vignette feather has a minimum of 0.01.
- Grading and filters work in linear RGB. `importCubeLut(text, options?)` adds a LUT adjustment and `setCubeLut(id, text)` replaces its table. LUTs operate on straight sRGB values. The importer accepts standalone 1D tables and 3D tables of up to 65 samples per axis, with tetrahedral interpolation for 3D. [Adobe Cube LUT specification](https://kono.phpage.fr/images/a/a1/Adobe-cube-lut-specification-1.0.pdf)
- `await editor.histogram(rect?)` returns 256 bins for each of red, green, blue, and Rec. 709 luma of the visible composite, along with the pixel count. It uses sRGB bytes and excludes pixels whose alpha byte is zero.

### Project files

`saveProject()` captures the document when it is called, so editing can continue while it runs. A `.nijimi` file is a zip with `manifest.json` and one entry per 256×256 tile, as `tiles/<layer id>/<x>_<y>` in little-endian RGBA16F with premultiplied alpha. Version 2 records the adjustment settings in the manifest, mask tiles under `masks/`, and LUT samples under `luts/`. Version 1 projects remain readable. `loadProject()` checks the whole file first and keeps the open document if anything is wrong. If several files are opened at once, the last call wins.

### GPU device loss

Pixels live on the GPU, so a lost device cannot be restored in place. The engine sends `deviceLost`, sets `isDeviceLost`, and every later change, export and save fails with "The GPU device was lost.". To recover, call `free()`, create a new editor on the same canvas and load the last saved project. Hosts should save regularly.

## Known limits

- The composited image is one RGBA16F texture with mipmaps (about 88 MB at 4K), in addition to the layer tiles. The largest side is the GPU texture limit.
- Synchronous GPU edits can emit `gpuError` after a change if the GPU runs out of memory. `loadProject` and `fill` reject on GPU allocation errors before committing.
- Saves read tiles in batches and compress them on the main thread.
- Each visible blur or sharpen also redraws the tiles next to a change, so painting slows down as they stack up.
- A project can have at most 1,000 layers, 16 nested groups and 32,768 tiles.
- Groups cannot pass through to the layers below.
- WebGPU is required. There is no WebGL fallback.

## Licence

Licensed under either [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
