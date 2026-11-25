import type {
  Command,
  Editor,
  LayerState,
  Point,
  RetouchTool,
  SelectionShape
} from '../pkg/nijimi.js';
import { element, icon } from './dom';
import { canvasPoint } from './gestures';
import type { IconName } from './icons';
import { options } from './options';
import type { Overlay } from './overlay';

const TOOLS = [
  { id: 'move', label: 'Move layer or selected pixels', key: 'v' },
  { id: 'rectangle', label: 'Rectangle selection', key: 'm' },
  { id: 'ellipse', label: 'Ellipse selection', key: 'o' },
  { id: 'lasso', label: 'Lasso selection', key: 'l' },
  { id: 'wand', label: 'Select similar pixels', key: 'w' },
  { id: 'brush', label: 'Brush', key: 'b' },
  { id: 'eraser', label: 'Eraser', key: 'e' },
  { id: 'fill', label: 'Fill', key: 'g' },
  { id: 'eyedropper', label: 'Pick a color', key: 'i' },
  { id: 'clone', label: 'Clone', key: 's' },
  { id: 'heal', label: 'Heal', key: 'j' },
  { id: 'smudge', label: 'Smudge', key: 'r' },
  { id: 'blur', label: 'Blur', key: 'u' },
  { id: 'text', label: 'Text', key: 't' },
  { id: 'hand', label: 'Pan', key: 'h' }
] as const satisfies readonly { id: IconName; label: string; key: string }[];

type ToolId = (typeof TOOLS)[number]['id'];

const BRUSHES: ToolId[] = ['brush', 'eraser', 'clone', 'heal', 'smudge', 'blur'];
const SHORTEST_SELECTION_PATH = 3;

export interface ToolHost {
  editor: Editor;
  canvas: HTMLCanvasElement;
  overlay: Overlay;
  activeLayer: () => LayerState | null;
  attempt: (work: () => void) => boolean;
  run: (work: () => Promise<void>) => void;
  dispatch: (command: Command) => void;
  addText: (point: Point) => void;
  pan: (dx: number, dy: number) => void;
  hint: (message: string) => void;
}

interface Gesture {
  drag: (event: PointerEvent) => void;
  release: (event: PointerEvent) => void;
  cancel: () => void;
}

type Drag = (start: Point, end: Point, path: Point[]) => void;
type Shape = (start: Point, end: Point, path: Point[]) => SelectionShape;

function pressure(event: PointerEvent) {
  return event.pointerType === 'pen' ? event.pressure : 1;
}

function box(start: Point, end: Point) {
  const [left, top] = [Math.min(start.x, end.x), Math.min(start.y, end.y)].map(Math.round);
  const [right, bottom] = [Math.max(start.x, end.x), Math.max(start.y, end.y)].map(Math.round);
  return { x: left, y: top, width: right - left, height: bottom - top };
}

export function createTools(host: ToolHost) {
  const { editor, canvas, overlay } = host;
  const rail = element('tools');
  const setSource = element<HTMLButtonElement>('set-source');
  const radios = new Map<ToolId, HTMLInputElement>();
  let current: ToolId = 'brush';
  let gesture: Gesture | null = null;
  let source: Point | null = null;
  let picksSource = false;

  function documentPoint(event: PointerEvent) {
    const { x, y } = canvasPoint(canvas, event);
    return editor.documentPoint(x, y);
  }

  function pixel(event: PointerEvent) {
    const point = documentPoint(event);
    const [x, y] = [Math.floor(point.x), Math.floor(point.y)];
    const isInside = x >= 0 && y >= 0 && x < editor.width && y < editor.height;
    return isInside ? { x, y } : null;
  }

  function stroke(
    event: PointerEvent,
    begin: (x: number, y: number, pressure: number) => void
  ): Gesture | null {
    const start = canvasPoint(canvas, event);
    if (!host.attempt(() => begin(start.x, start.y, pressure(event)))) return null;
    return {
      drag(next) {
        const samples = next.getCoalescedEvents?.() ?? [];
        host.attempt(() => {
          for (const sample of samples.length ? samples : [next]) {
            const { x, y } = canvasPoint(canvas, sample);
            editor.extendStroke(x, y, pressure(sample));
          }
        });
      },
      release: () => host.attempt(() => editor.endStroke()),
      cancel: () => host.attempt(() => editor.cancelStroke())
    };
  }

  function retouch(event: PointerEvent, tool: RetouchTool) {
    return stroke(event, (...sample) => editor.beginRetouchStroke(tool, ...sample));
  }

  function copy(event: PointerEvent, type: 'clone' | 'heal') {
    const start = documentPoint(event);
    if (event.altKey || picksSource) {
      source = start;
      picksSource = false;
      updateSource();
      host.hint('Source set. Paint where the copy should go.');
      return null;
    }
    if (!source) {
      host.hint('Choose what to copy first: Alt-click it, or press Set source and tap it.');
      return null;
    }
    const offsetX = Math.round(source.x - start.x);
    const offsetY = Math.round(source.y - start.y);
    return retouch(event, { type, offsetX, offsetY });
  }

  function select(event: PointerEvent, show: Drag, shape: Shape): Gesture {
    const mode = options.selectionMode(event);
    const start = documentPoint(event);
    const path = [start];
    const clear = () => overlay.showPolygon([]);
    return {
      drag(next) {
        const end = documentPoint(next);
        path.push(end);
        show(start, end, path);
      },
      release(last) {
        clear();
        const isClick = path.length < SHORTEST_SELECTION_PATH;
        if (isClick) host.dispatch({ type: 'deselect' });
        else host.dispatch({ type: 'select', shape: shape(start, documentPoint(last), path), mode });
      },
      cancel: clear
    };
  }

  function showBox(start: Point, end: Point) {
    overlay.showPolygon([start, { x: end.x, y: start.y }, end, { x: start.x, y: end.y }]);
  }

  function move(event: PointerEvent): Gesture | null {
    const layer = host.activeLayer();
    if (!layer) return null;
    const start = documentPoint(event);
    return {
      drag(next) {
        const { x, y } = documentPoint(next);
        const transform = { x: Math.round(x - start.x), y: Math.round(y - start.y) };
        host.attempt(() => editor.preview({ type: 'transformLayer', id: layer.id, transform }));
      },
      release: () => host.attempt(() => editor.commitPreview()),
      cancel: () => host.attempt(() => editor.cancelPreview())
    };
  }

  function pan(event: PointerEvent): Gesture {
    let last = canvasPoint(canvas, event);
    return {
      drag(next) {
        const point = canvasPoint(canvas, next);
        host.pan(point.x - last.x, point.y - last.y);
        last = point;
      },
      release: () => {},
      cancel: () => {}
    };
  }

  function fill(event: PointerEvent) {
    const layer = host.activeLayer();
    const at = pixel(event);
    if (!layer || !at) return;
    const color: [number, number, number, number] = [...options.color(), 1];
    host.run(() => editor.fill({ id: layer.id, ...at, color, tolerance: options.tolerance() }));
  }

  function selectSimilar(event: PointerEvent) {
    const at = pixel(event);
    if (!at) return;
    const settings = {
      tolerance: options.tolerance(),
      contiguous: options.contiguous(),
      mode: options.selectionMode(event)
    };
    host.run(() => editor.selectSimilar({ ...at, ...settings }));
  }

  function pickColor(event: PointerEvent) {
    const at = pixel(event);
    if (at) host.run(async () => options.pickColor(await editor.pickColor(at.x, at.y)));
  }

  const starts: Record<ToolId, (event: PointerEvent) => Gesture | null | void> = {
    brush(event) {
      editor.setEraser(false);
      return stroke(event, (...sample) => editor.beginStroke(...sample));
    },
    eraser(event) {
      editor.setEraser(true);
      return stroke(event, (...sample) => editor.beginStroke(...sample));
    },
    clone: (event) => copy(event, 'clone'),
    heal: (event) => copy(event, 'heal'),
    smudge: (event) => retouch(event, { type: 'smudge' }),
    blur: (event) => retouch(event, { type: 'blur', radius: options.blurRadius() }),
    rectangle: (event) =>
      select(event, showBox, (start, end) => ({ type: 'rectangle', ...box(start, end) })),
    ellipse: (event) =>
      select(event, overlay.showEllipse, (start, end) => ({ type: 'ellipse', ...box(start, end) })),
    lasso: (event) =>
      select(
        event,
        (_start, _end, path) => overlay.showPolygon(path),
        (_start, _end, points) => ({ type: 'lasso', points })
      ),
    wand: selectSimilar,
    fill,
    eyedropper: pickColor,
    move,
    hand: pan,
    text: (event) => host.addText(documentPoint(event))
  };

  function updateSource() {
    const isCopying = current === 'clone' || current === 'heal';
    overlay.showSource(isCopying ? source : null);
    setSource.ariaPressed = String(picksSource);
  }

  function showCursor(event: PointerEvent | null) {
    if (!event || event.pointerType === 'touch' || !BRUSHES.includes(current)) {
      overlay.showCursor(null, 0);
      return;
    }
    const rect = canvas.getBoundingClientRect();
    const position = { x: event.clientX - rect.left, y: event.clientY - rect.top };
    overlay.showCursor(position, options.brushSize());
  }

  function choose(tool: ToolId) {
    current = tool;
    picksSource = false;
    canvas.dataset.tool = tool;
    const radio = radios.get(tool);
    if (radio) radio.checked = true;
    options.showFor(tool);
    showCursor(null);
    updateSource();
  }

  for (const { id, label, key } of TOOLS) {
    const radio = document.createElement('input');
    Object.assign(radio, { type: 'radio', name: 'tool', value: id, ariaLabel: label });
    radio.addEventListener('change', () => choose(id));
    radios.set(id, radio);
    const button = document.createElement('label');
    button.title = `${label} (${key.toUpperCase()})`;
    button.append(radio, icon(id));
    rail.append(button);
  }
  setSource.addEventListener('click', () => {
    picksSource = !picksSource;
    updateSource();
  });
  canvas.addEventListener('pointermove', showCursor);
  canvas.addEventListener('pointerleave', () => showCursor(null));
  choose(current);

  return {
    updateSource,
    chooseByKey(key: string) {
      const tool = TOOLS.find((candidate) => candidate.key === key);
      if (tool) choose(tool.id);
    },
    press(event: PointerEvent) {
      gesture = starts[current](event) ?? null;
      return gesture !== null;
    },
    drag: (event: PointerEvent) => gesture?.drag(event),
    release(event: PointerEvent) {
      gesture?.release(event);
      gesture = null;
    },
    cancel() {
      gesture?.cancel();
      gesture = null;
    }
  };
}
