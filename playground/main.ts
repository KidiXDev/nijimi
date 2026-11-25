import init, { Editor, type Command, type EditorEvent } from '../pkg/nijimi.js';
import { benchmark } from './benchmark';
import { element, fillIcons } from './dom';
import { exportPng, openFile, saveProject } from './files';
import { listenForGestures } from './gestures';
import { createLayerPanel } from './layers';
import { bindOptions, options } from './options';
import { createOverlay } from './overlay';
import { createTextPanel } from './text';
import { createTools } from './tools';

const DEFAULT_WIDTH = 3840;
const DEFAULT_HEIGHT = 2160;

const canvas = element<HTMLCanvasElement>('canvas');
const status = element<HTMLOutputElement>('status');
const view = element<HTMLOutputElement>('view');
const fileInput = element<HTMLInputElement>('file');
const undoButton = element<HTMLButtonElement>('undo');
const redoButton = element<HTMLButtonElement>('redo');
const panelToggle = element<HTMLButtonElement>('toggle-panel');
const selectionButtons = document.querySelectorAll<HTMLButtonElement>('[data-needs-selection]');
const maskSelectButton = element<HTMLButtonElement>('select-mask');

function showMessage(message: string, isError = false) {
  status.value = message;
  status.classList.toggle('error', isError);
}

function showError(error: unknown) {
  showMessage(error instanceof Error ? error.message : String(error), true);
}

function canvasPixels() {
  const { width, height } = canvas.getBoundingClientRect();
  return {
    width: Math.max(1, Math.round(width * devicePixelRatio)),
    height: Math.max(1, Math.round(height * devicePixelRatio))
  };
}

function isEditingText(target: EventTarget | null) {
  if (target instanceof HTMLInputElement) return ['text', 'number'].includes(target.type);
  return target instanceof HTMLTextAreaElement || target instanceof HTMLSelectElement;
}

async function main() {
  if (!navigator.gpu) throw new Error('WebGPU is unavailable in this browser.');
  fillIcons();
  await init();
  Object.assign(canvas, canvasPixels());
  const editor = await Editor.create(canvas, DEFAULT_WIDTH, DEFAULT_HEIGHT);
  const overlay = createOverlay(element('overlay'), editor);

  let autoFit = true;
  let renderQueued = false;
  function requestRender() {
    if (renderQueued) return;
    renderQueued = true;
    requestAnimationFrame(() => {
      renderQueued = false;
      try {
        editor.render();
        tools.updateSource();
        view.value = `${editor.width} × ${editor.height} at ${Math.round(editor.zoom * 100)}%`;
      } catch (error) {
        showError(error);
      }
    });
  }

  function attempt(work: () => void) {
    try {
      work();
      requestRender();
      return true;
    } catch (error) {
      showError(error);
      return false;
    }
  }

  function run(work: () => Promise<void>) {
    showMessage('Working…');
    work().then(() => {
      showMessage('');
      requestRender();
    }, showError);
  }

  function dispatch(command: Command) {
    if (attempt(() => editor.dispatch(command))) showMessage('');
  }

  const layerPanel = createLayerPanel(
    () => editor.state(),
    dispatch,
    (command) => attempt(() => editor.preview(command)),
    () => attempt(() => editor.commitPreview())
  );
  const textPanel = createTextPanel(editor, layerPanel.activeLayer, attempt);

  function pan(dx: number, dy: number) {
    autoFit = false;
    editor.panBy(dx, dy);
    requestRender();
  }

  const tools = createTools({
    editor,
    canvas,
    overlay,
    activeLayer: layerPanel.activeLayer,
    attempt,
    run,
    dispatch,
    addText: (point) => textPanel.add(point),
    pan,
    hint: showMessage
  });

  const gestures = listenForGestures(canvas, {
    press: tools.press,
    drag: tools.drag,
    release: tools.release,
    cancel: tools.cancel,
    pan,
    zoom(center, factor) {
      autoFit = false;
      editor.zoomAt(center.x, center.y, factor);
      requestRender();
    }
  });

  function updatePanels() {
    const { canUndo, canRedo, selection } = editor.state();
    undoButton.disabled = !canUndo;
    redoButton.disabled = !canRedo;
    for (const button of selectionButtons) button.disabled = !selection;
    maskSelectButton.disabled = !layerPanel.activeLayer()?.mask;
    layerPanel.update();
    textPanel.update();
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

  bindOptions(editor, attempt);

  // Selection actions
  function selectAll() {
    const { width, height } = editor;
    dispatch({ type: 'select', shape: { type: 'rectangle', x: 0, y: 0, width, height } });
  }

  function clearSelected() {
    const layer = layerPanel.activeLayer();
    if (!layer || !editor.state().selection) return;
    run(() => editor.fill({ id: layer.id, x: 0, y: 0, color: [0, 0, 0, 0], tolerance: 1 }));
  }

  function maskFromSelection() {
    const layer = layerPanel.activeLayer();
    if (layer) dispatch({ type: 'maskFromSelection', id: layer.id });
  }

  function selectMask() {
    const layer = layerPanel.activeLayer();
    if (layer) run(() => editor.selectMask(layer.id));
  }

  const actions: Record<string, () => void> = {
    'select-all': selectAll,
    deselect: () => dispatch({ type: 'deselect' }),
    invert: () => dispatch({ type: 'invertSelection' }),
    feather: () => dispatch({ type: 'featherSelection', radius: options.edgeRadius() }),
    grow: () => dispatch({ type: 'growSelection', radius: options.edgeRadius() }),
    shrink: () => dispatch({ type: 'shrinkSelection', radius: options.edgeRadius() }),
    'clear-selected': clearSelected,
    'mask-from-selection': maskFromSelection,
    'select-mask': selectMask,
    undo: () => dispatch({ type: 'undo' }),
    redo: () => dispatch({ type: 'redo' }),
    save: () => run(() => saveProject(editor)),
    export: () => run(() => exportPng(editor)),
    fit() {
      autoFit = true;
      editor.fit();
      requestRender();
    }
  };
  for (const [id, action] of Object.entries(actions)) element(id).addEventListener('click', action);

  // Keyboard
  const commandKeys: Record<string, () => void> = {
    z: actions.undo,
    'shift+z': actions.redo,
    y: actions.redo,
    a: actions['select-all'],
    d: actions.deselect,
    'shift+i': actions.invert
  };
  const plainKeys: Record<string, () => void> = {
    '[': () => options.resizeBrush(-1),
    ']': () => options.resizeBrush(1),
    delete: clearSelected,
    backspace: clearSelected
  };

  addEventListener('keydown', (event) => {
    if (event.key === 'Escape') {
      gestures.cancel();
      attempt(() => editor.cancelPreview());
      return;
    }
    if (isEditingText(event.target)) return;
    const key = event.key.toLowerCase();
    if (event.ctrlKey || event.metaKey) {
      const action = commandKeys[event.shiftKey ? `shift+${key}` : key];
      if (!action) return;
      event.preventDefault();
      action();
    } else if (!event.altKey) {
      const action = plainKeys[key];
      if (action) action();
      else tools.chooseByKey(key);
    }
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
    showMessage(`Opening ${file.name}…`);
    try {
      await openFile(editor, file);
      showMessage('');
      autoFit = true;
      requestRender();
    } catch (error) {
      showError(error);
    }
    fileInput.value = '';
  });

  panelToggle.addEventListener('click', () => {
    const isOpen = panelToggle.ariaExpanded !== 'true';
    panelToggle.ariaExpanded = String(isOpen);
    document.body.classList.toggle('panel-open', isOpen);
  });

  element('benchmark').addEventListener('click', () => {
    benchmark(editor, canvas).then((result) => {
      showMessage(
        [
          `median ${result.medianFrameMs.toFixed(1)} ms`,
          `p99 ${result.p99FrameMs.toFixed(1)} ms`,
          `CPU ${result.cpuMsPerFrame.toFixed(2)} ms`,
          `uncapped ${result.uncappedMsPerFrame.toFixed(2)} ms`,
          `readback ${result.readbackMs.toFixed(0)} ms`,
          `undo ${result.undoMs.toFixed(1)} ms`
        ].join(', ')
      );
    }, showError);
  });

  showMessage('');
  updatePanels();
  requestRender();
}

main().catch(showError);
