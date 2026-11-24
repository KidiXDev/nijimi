# Nijimi

An open-source, GPU-accelerated, headless image editing engine for the web.

[Try the playground](https://kidixdev.github.io/nijimi/) (needs a browser with WebGPU)

- **GPU-accelerated:** written in Rust, compiled to WebAssembly and rendered with WebGPU.
- **Headless:** no toolbar, panels or framework. You give it a canvas and build your own interface on its API.
- **For the web:** works in any web app or desktop webview that supports WebGPU.

Status: proof of concept. It opens an image, paints with a soft brush, pans, zooms and exports. The API will change.

## Layout

| Path | Purpose |
|---|---|
| `crates/core` | Viewport and brush strokes, with no GPU or web code |
| `crates/render` | wgpu rendering and pixel readback |
| `crates/wasm` | JavaScript bindings |
| `playground` | Vite page for trying the engine |
| `xtask` | Builds the WASM package into `pkg/` |

## Commands

Requires Rust and Bun. `rust-toolchain.toml` adds the WASM target.

```bash
bun install
bun run dev        # build the engine and open the playground on :1430
bun run wasm       # rebuild the engine only
bun run test       # unit tests and GPU tests (needs a GPU adapter)
bun run lint
bun run typecheck
```

In the playground, paint with the left button, pan with the middle button and zoom with the wheel.

## Usage

```ts
import init, { Editor } from 'nijimi';

await init();
const editor = await Editor.create(canvas, 1920, 1080);
editor.setBrush(64, 0.5, 1, 0.9, 0.2, 0.4);
editor.beginStroke(100, 100, 1);
editor.extendStroke(300, 200, 1);
editor.endStroke();
editor.render();
```

Coordinates are canvas pixels. Colors and pixels use sRGB values. Vite apps must add `nijimi` to `optimizeDeps.exclude`, otherwise the dev server cannot find the WASM file.

## Known limits

- The document is one full-size RGBA16F texture. Sparse tiles, layers and undo come next.
- Zooming far out has no mipmaps, so fine detail can shimmer.
- WebGPU is required. There is no WebGL fallback.
- A lost GPU device is not recovered.

## Licence

Licensed under either [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
