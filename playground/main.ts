import init, { Editor } from '../pkg/nijimi.js';

const DEFAULT_WIDTH = 3840;
const DEFAULT_HEIGHT = 2160;
const ZOOM_SPEED = 0.0015;
const BENCHMARK_FRAMES = 240;
const BENCHMARK_POINTS_PER_FRAME = 8;

function element<T extends HTMLElement>(id: string) {
  const found = document.getElementById(id);
  if (!found) throw new Error(`Missing #${id}`);
  return found as T;
}

const canvas = element<HTMLCanvasElement>('canvas');
const status = element<HTMLOutputElement>('status');
const fileInput = element<HTMLInputElement>('file');
const sizeInput = element<HTMLInputElement>('size');
const hardnessInput = element<HTMLInputElement>('hardness');
const flowInput = element<HTMLInputElement>('flow');
const colorInput = element<HTMLInputElement>('color');

function showError(error: unknown) {
  status.value = error instanceof Error ? error.message : String(error);
}

function canvasPixels() {
  const { width, height } = canvas.getBoundingClientRect();
  return {
    width: Math.max(1, Math.round(width * devicePixelRatio)),
    height: Math.max(1, Math.round(height * devicePixelRatio))
  };
}

function canvasPoint(event: MouseEvent) {
  const rect = canvas.getBoundingClientRect();
  return {
    x: (event.clientX - rect.left) * (canvas.width / rect.width),
    y: (event.clientY - rect.top) * (canvas.height / rect.height)
  };
}

function pressure(event: PointerEvent) {
  return event.pointerType === 'pen' ? event.pressure : 1;
}

async function decodeRgba(file: File) {
  const bitmap = await createImageBitmap(file);
  const { width, height } = bitmap;
  const context = new OffscreenCanvas(width, height).getContext('2d');
  if (!context) throw new Error('2D canvas is unavailable');
  context.drawImage(bitmap, 0, 0);
  bitmap.close();
  return context.getImageData(0, 0, width, height);
}

async function exportPng(editor: Editor) {
  const { width, height } = editor;
  const rgba = await editor.exportRgba();
  const output = new OffscreenCanvas(width, height);
  output
    .getContext('2d')
    ?.putImageData(
      new ImageData(new Uint8ClampedArray(rgba), width, height),
      0,
      0
    );
  const url = URL.createObjectURL(await output.convertToBlob());
  const link = document.createElement('a');
  link.href = url;
  link.download = 'nijimi.png';
  link.click();
  URL.revokeObjectURL(url);
}

function nextFrame() {
  return new Promise<number>((resolve) => requestAnimationFrame(resolve));
}

async function benchmark(editor: Editor) {
  const intervals: number[] = [];
  let cpuTime = 0;
  let previous = await nextFrame();
  editor.beginStroke(canvas.width / 2, canvas.height / 2, 1);
  for (let frame = 0; frame < BENCHMARK_FRAMES; frame += 1) {
    const started = performance.now();
    for (let step = 0; step < BENCHMARK_POINTS_PER_FRAME; step += 1) {
      const angle = (frame + step / BENCHMARK_POINTS_PER_FRAME) * 0.15;
      const radius = (Math.min(canvas.width, canvas.height) / 2) * 0.9;
      editor.extendStroke(
        canvas.width / 2 + Math.cos(angle) * radius,
        canvas.height / 2 + Math.sin(angle * 1.7) * radius,
        1
      );
    }
    editor.render();
    cpuTime += performance.now() - started;
    const now = await nextFrame();
    intervals.push(now - previous);
    previous = now;
  }
  editor.endStroke();
  const readbackStarted = performance.now();
  await editor.exportRgba();
  intervals.sort((a, b) => a - b);
  return {
    frames: BENCHMARK_FRAMES,
    medianFrameMs: intervals[Math.floor(intervals.length * 0.5)],
    p99FrameMs: intervals[Math.floor(intervals.length * 0.99)],
    cpuMsPerFrame: cpuTime / BENCHMARK_FRAMES,
    drainAndReadbackMs: performance.now() - readbackStarted
  };
}

async function main() {
  if (!navigator.gpu) throw new Error('WebGPU is unavailable in this browser');
  await init();
  Object.assign(canvas, canvasPixels());
  const editor = await Editor.create(canvas, DEFAULT_WIDTH, DEFAULT_HEIGHT);

  let renderQueued = false;
  function requestRender() {
    if (renderQueued) return;
    renderQueued = true;
    requestAnimationFrame(() => {
      renderQueued = false;
      try {
        editor.render();
        status.value = `${editor.width}×${editor.height} at ${Math.round(editor.zoom * 100)}%`;
      } catch (error) {
        showError(error);
      }
    });
  }

  function applyBrush() {
    const channel = (start: number) =>
      parseInt(colorInput.value.slice(start, start + 2), 16) / 255;
    editor.setBrush(
      sizeInput.valueAsNumber,
      hardnessInput.valueAsNumber,
      flowInput.valueAsNumber,
      channel(1),
      channel(3),
      channel(5)
    );
  }

  for (const input of [sizeInput, hardnessInput, flowInput, colorInput])
    input.addEventListener('input', applyBrush);
  applyBrush();

  let gesture: 'paint' | 'pan' | null = null;
  let autoFit = true;

  canvas.addEventListener('pointerdown', (event) => {
    if (gesture || (event.button !== 0 && event.button !== 1)) return;
    event.preventDefault();
    canvas.setPointerCapture(event.pointerId);
    gesture = event.button === 1 ? 'pan' : 'paint';
    if (gesture === 'paint') {
      const { x, y } = canvasPoint(event);
      editor.beginStroke(x, y, pressure(event));
      requestRender();
    }
  });

  canvas.addEventListener('pointermove', (event) => {
    if (!gesture) return;
    if (gesture === 'pan') {
      autoFit = false;
      editor.panBy(
        event.movementX * devicePixelRatio,
        event.movementY * devicePixelRatio
      );
    } else {
      const samples = event.getCoalescedEvents();
      for (const sample of samples.length ? samples : [event]) {
        const { x, y } = canvasPoint(sample);
        editor.extendStroke(x, y, pressure(sample));
      }
    }
    requestRender();
  });

  canvas.addEventListener('lostpointercapture', () => {
    gesture = null;
    editor.endStroke();
  });

  canvas.addEventListener(
    'wheel',
    (event) => {
      event.preventDefault();
      const { x, y } = canvasPoint(event);
      autoFit = false;
      editor.zoomAt(x, y, Math.exp(-event.deltaY * ZOOM_SPEED));
      requestRender();
    },
    { passive: false }
  );

  new ResizeObserver(() => {
    const { width, height } = canvasPixels();
    editor.resize(width, height);
    if (autoFit) editor.fit();
    requestRender();
  }).observe(canvas);

  fileInput.addEventListener('change', async () => {
    const file = fileInput.files?.[0];
    if (!file) return;
    try {
      const { width, height, data } = await decodeRgba(file);
      editor.loadRgba(width, height, new Uint8Array(data.buffer));
      autoFit = true;
      requestRender();
    } catch (error) {
      showError(error);
    }
  });

  element('fit').addEventListener('click', () => {
    autoFit = true;
    editor.fit();
    requestRender();
  });

  element('export').addEventListener('click', () => {
    exportPng(editor).catch(showError);
  });

  element('benchmark').addEventListener('click', async () => {
    try {
      const result = await benchmark(editor);
      status.value = `median ${result.medianFrameMs.toFixed(1)} ms, p99 ${result.p99FrameMs.toFixed(1)} ms, CPU ${result.cpuMsPerFrame.toFixed(2)} ms, readback ${result.drainAndReadbackMs.toFixed(0)} ms`;
    } catch (error) {
      showError(error);
    }
  });

  requestRender();
}

main().catch(showError);
