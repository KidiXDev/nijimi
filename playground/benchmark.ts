import type { BlendMode, Editor } from '../pkg/nijimi.js';

const WIDTH = 3840;
const HEIGHT = 2160;
const LAYERS = 20;
const GROUPED_LAYERS = 4;
const STROKES_PER_LAYER = 3;
const FRAMES = 240;
const UNCAPPED_FRAMES = 240;
const POINTS_PER_FRAME = 8;
const BLEND_MODES: BlendMode[] = [
  'normal',
  'multiply',
  'normal',
  'screen',
  'overlay',
  'normal',
  'softLight',
  'add',
  'darken',
  'lighten'
];

function nextFrame() {
  return new Promise<number>((resolve) => requestAnimationFrame(resolve));
}

function fraction(seed: number) {
  const value = Math.sin(seed * 12.9898) * 43758.5453;
  return value - Math.floor(value);
}

function buildDocument(editor: Editor) {
  editor.loadRgba(WIDTH, HEIGHT, new Uint8Array(WIDTH * HEIGHT * 4));
  const layers = [editor.state().layers[0].id];
  const group = editor.dispatch({ type: 'addGroup' });
  while (layers.length < LAYERS) {
    const parent = layers.length <= GROUPED_LAYERS ? group : null;
    const id = editor.dispatch({ type: 'addLayer', parent });
    if (id !== undefined) layers.push(id);
  }
  for (const [index, layer] of layers.entries()) {
    editor.dispatch({
      type: 'setLayerBlendMode',
      id: layer,
      blendMode: BLEND_MODES[index % BLEND_MODES.length]
    });
    for (let stroke = 0; stroke < STROKES_PER_LAYER; stroke += 1) {
      const seed = index * 10 + stroke;
      const point = (offset: number) => ({
        x: fraction(seed + offset) * WIDTH,
        y: fraction(seed + offset + 0.5) * HEIGHT
      });
      editor.dispatch({
        type: 'paintStroke',
        layer,
        brush: {
          diameter: 500,
          hardness: 0.3,
          flow: 0.6,
          opacity: 0.7,
          color: [fraction(seed + 1), fraction(seed + 2), fraction(seed + 3)]
        },
        points: [point(0.1), point(0.2)]
      });
    }
  }
  editor.dispatch({ type: 'selectLayer', id: layers[LAYERS / 2] });
  editor.fit();
}

function paintFrame(editor: Editor, canvas: HTMLCanvasElement, frame: number) {
  const radius = (Math.min(canvas.width, canvas.height) / 2) * 0.9;
  for (let step = 0; step < POINTS_PER_FRAME; step += 1) {
    const angle = (frame + step / POINTS_PER_FRAME) * 0.15;
    editor.extendStroke(
      canvas.width / 2 + Math.cos(angle) * radius,
      canvas.height / 2 + Math.sin(angle * 1.7) * radius,
      1
    );
  }
  editor.render();
}

async function timed(work: () => unknown) {
  const started = performance.now();
  await work();
  return performance.now() - started;
}

export async function benchmark(editor: Editor, canvas: HTMLCanvasElement) {
  buildDocument(editor);
  editor.render();
  await editor.exportRgba();

  const intervals: number[] = [];
  let cpuTime = 0;
  let previous = await nextFrame();
  editor.beginStroke(canvas.width / 2, canvas.height / 2, 1);
  for (let frame = 0; frame < FRAMES; frame += 1) {
    const started = performance.now();
    paintFrame(editor, canvas, frame);
    cpuTime += performance.now() - started;
    const now = await nextFrame();
    intervals.push(now - previous);
    previous = now;
  }
  editor.endStroke();
  intervals.sort((a, b) => a - b);

  const readbackMs = await timed(() => editor.exportRgba());
  editor.beginStroke(canvas.width / 2, canvas.height / 2, 1);
  const uncappedMs = await timed(() => {
    for (let frame = 0; frame < UNCAPPED_FRAMES; frame += 1) paintFrame(editor, canvas, frame);
    return editor.exportRgba();
  });
  editor.endStroke();
  const undoMs = await timed(() => editor.dispatch({ type: 'undo' }));
  editor.render();
  await nextFrame();
  await nextFrame();

  return {
    frames: FRAMES,
    medianFrameMs: intervals[Math.floor(intervals.length * 0.5)],
    p99FrameMs: intervals[Math.floor(intervals.length * 0.99)],
    cpuMsPerFrame: cpuTime / FRAMES,
    uncappedMsPerFrame: Math.max(0, uncappedMs - readbackMs) / UNCAPPED_FRAMES,
    readbackMs,
    undoMs
  };
}
