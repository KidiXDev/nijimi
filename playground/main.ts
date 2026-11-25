import init, { Editor, type Command, type EditorEvent } from '../pkg/nijimi.js';
import { benchmark } from './benchmark';
import { element } from './dom';
import { createLayerPanel } from './layers';

const DEFAULT_WIDTH = 3840;
const DEFAULT_HEIGHT = 2160;
const ZOOM_SPEED = 0.0015;
const PROJECT_EXTENSION = '.nijimi';

const canvas = element<HTMLCanvasElement>('canvas');
const status = element<HTMLOutputElement>('status');
const fileInput = element<HTMLInputElement>('file');
const sizeInput = element<HTMLInputElement>('size');
const hardnessInput = element<HTMLInputElement>('hardness');
const flowInput = element<HTMLInputElement>('flow');
const brushOpacityInput = element<HTMLInputElement>('brush-opacity');
const colorInput = element<HTMLInputElement>('color');
const undoButton = element<HTMLButtonElement>('undo');
const redoButton = element<HTMLButtonElement>('redo');

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

async function openFile(editor: Editor, file: File) {
  if (file.name.toLowerCase().endsWith(PROJECT_EXTENSION)) {
    await editor.loadProject(await file.bytes());
    return;
  }
  const { width, height, data } = await decodeRgba(file);
  editor.loadRgba(width, height, new Uint8Array(data.buffer));
}

function download(blob: Blob, name: string) {
  const url = URL.createObjectURL(blob);
  const link = document.createElement('a');
  link.href = url;
  link.download = name;
  link.click();
  URL.revokeObjectURL(url);
}

async function exportPng(editor: Editor) {
  const { width, height } = editor;
  const rgba = await editor.exportRgba();
  const output = new OffscreenCanvas(width, height);
  output
    .getContext('2d')
    ?.putImageData(new ImageData(new Uint8ClampedArray(rgba), width, height), 0, 0);
  download(await output.convertToBlob(), 'nijimi.png');
}

async function saveProject(editor: Editor) {
  const file = await editor.saveProject();
  download(new Blob([new Uint8Array(file)]), `drawing${PROJECT_EXTENSION}`);
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

  function attempt(work: () => void) {
    try {
      work();
      requestRender();
    } catch (error) {
      showError(error);
    }
  }

  function dispatch(command: Command) {
    attempt(() => editor.dispatch(command));
    layerPanel.update();
  }

  const layerPanel = createLayerPanel(
    () => editor.state(),
    dispatch,
    (command) => attempt(() => editor.preview(command)),
    () => attempt(() => editor.commitPreview())
  );

  function updatePanels() {
    const { canUndo, canRedo } = editor.state();
    undoButton.disabled = !canUndo;
    redoButton.disabled = !canRedo;
    layerPanel.update();
  }

  editor.subscribe((event: EditorEvent) => {
    if (event.type === 'pixelsChanged') return;
    if (event.type === 'deviceLost') {
      showError(`The GPU device was lost. Reload the page to continue. ${event.message}`);
      return;
    }
    if (event.type === 'gpuError') {
      showError(event.message);
      return;
    }
    updatePanels();
    requestRender();
  });

  function applyBrush() {
    const channel = (start: number) => parseInt(colorInput.value.slice(start, start + 2), 16) / 255;
    attempt(() =>
      editor.setBrush({
        diameter: sizeInput.valueAsNumber,
        hardness: hardnessInput.valueAsNumber,
        flow: flowInput.valueAsNumber,
        opacity: brushOpacityInput.valueAsNumber,
        color: [channel(1), channel(3), channel(5)]
      })
    );
  }

  for (const input of [sizeInput, hardnessInput, flowInput, brushOpacityInput, colorInput])
    input.addEventListener('input', applyBrush);
  applyBrush();

  let gesture: 'paint' | 'pan' | null = null;
  let autoFit = true;

  canvas.addEventListener('pointerdown', (event) => {
    if (gesture || (event.button !== 0 && event.button !== 1)) return;
    event.preventDefault();
    if (event.button === 1) {
      gesture = 'pan';
    } else {
      attempt(() => {
        const { x, y } = canvasPoint(event);
        editor.beginStroke(x, y, pressure(event));
        gesture = 'paint';
      });
    }
    if (gesture) canvas.setPointerCapture(event.pointerId);
  });

  canvas.addEventListener('pointermove', (event) => {
    if (!gesture) return;
    if (gesture === 'pan') {
      autoFit = false;
      editor.panBy(event.movementX * devicePixelRatio, event.movementY * devicePixelRatio);
    } else {
      const samples = event.getCoalescedEvents?.() ?? [];
      for (const sample of samples.length ? samples : [event]) {
        const { x, y } = canvasPoint(sample);
        editor.extendStroke(x, y, pressure(sample));
      }
    }
    requestRender();
  });

  canvas.addEventListener('pointercancel', () => {
    if (gesture === 'paint') attempt(() => editor.cancelStroke());
    gesture = null;
  });

  canvas.addEventListener('lostpointercapture', () => {
    if (gesture === 'paint') attempt(() => editor.endStroke());
    gesture = null;
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

  addEventListener('keydown', (event) => {
    if (event.key === 'Escape') {
      if (gesture === 'paint') {
        gesture = null;
        attempt(() => editor.cancelStroke());
      }
      attempt(() => editor.cancelPreview());
      return;
    }
    if (!(event.ctrlKey || event.metaKey) || event.target instanceof HTMLInputElement) return;
    const key = event.key.toLowerCase();
    const redo = key === 'y' || (key === 'z' && event.shiftKey);
    if (!redo && key !== 'z') return;
    event.preventDefault();
    dispatch({ type: redo ? 'redo' : 'undo' });
  });

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
      await openFile(editor, file);
      autoFit = true;
      requestRender();
    } catch (error) {
      showError(error);
    }
    fileInput.value = '';
  });

  undoButton.addEventListener('click', () => dispatch({ type: 'undo' }));
  redoButton.addEventListener('click', () => dispatch({ type: 'redo' }));

  element('fit').addEventListener('click', () => {
    autoFit = true;
    editor.fit();
    requestRender();
  });

  element('save').addEventListener('click', () => {
    saveProject(editor).catch(showError);
  });

  element('export').addEventListener('click', () => {
    exportPng(editor).catch(showError);
  });

  element('benchmark').addEventListener('click', async () => {
    try {
      const result = await benchmark(editor, canvas);
      status.value = [
        `median ${result.medianFrameMs.toFixed(1)} ms`,
        `p99 ${result.p99FrameMs.toFixed(1)} ms`,
        `CPU ${result.cpuMsPerFrame.toFixed(2)} ms`,
        `uncapped ${result.uncappedMsPerFrame.toFixed(2)} ms`,
        `readback ${result.readbackMs.toFixed(0)} ms`,
        `undo ${result.undoMs.toFixed(1)} ms`
      ].join(', ');
    } catch (error) {
      showError(error);
    }
  });

  updatePanels();
  requestRender();
}

main().catch(showError);
