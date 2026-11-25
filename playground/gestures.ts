import type { Point } from '../pkg/nijimi.js';

export interface GestureHandlers {
  press: (event: PointerEvent) => boolean;
  drag: (event: PointerEvent) => void;
  release: (event: PointerEvent) => void;
  cancel: () => void;
  pan: (dx: number, dy: number) => void;
  zoom: (center: Point, factor: number) => void;
}

const ZOOM_SPEED = 0.0015;

export function canvasPoint(
  canvas: HTMLCanvasElement,
  event: { clientX: number; clientY: number }
): Point {
  const rect = canvas.getBoundingClientRect();
  return {
    x: (event.clientX - rect.left) * (canvas.width / rect.width),
    y: (event.clientY - rect.top) * (canvas.height / rect.height)
  };
}

function pinch([a, b]: Point[]) {
  return {
    center: { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 },
    distance: Math.max(1, Math.hypot(a.x - b.x, a.y - b.y))
  };
}

export function listenForGestures(canvas: HTMLCanvasElement, handlers: GestureHandlers) {
  const touches = new Map<number, Point>();
  let active: { mode: 'tool' | 'pan'; pointer: number; last: Point } | null = null;
  let isPinching = false;

  function cancel() {
    if (active?.mode === 'tool') handlers.cancel();
    active = null;
  }

  function movePinch(event: PointerEvent) {
    const before = pinch([...touches.values()]);
    touches.set(event.pointerId, canvasPoint(canvas, event));
    const after = pinch([...touches.values()]);
    handlers.pan(after.center.x - before.center.x, after.center.y - before.center.y);
    handlers.zoom(after.center, after.distance / before.distance);
  }

  canvas.addEventListener('pointerdown', (event) => {
    const point = canvasPoint(canvas, event);
    if (event.pointerType === 'touch') {
      touches.set(event.pointerId, point);
      if (touches.size === 2) {
        cancel();
        isPinching = true;
      }
    }
    if (isPinching || active || (event.button !== 0 && event.button !== 1)) return;
    event.preventDefault();
    const mode = event.button === 1 ? 'pan' : 'tool';
    if (mode === 'tool' && !handlers.press(event)) return;
    active = { mode, pointer: event.pointerId, last: point };
    canvas.setPointerCapture(event.pointerId);
  });

  canvas.addEventListener('pointermove', (event) => {
    if (isPinching) {
      if (touches.size === 2 && touches.has(event.pointerId)) movePinch(event);
      return;
    }
    if (active?.pointer !== event.pointerId) return;
    if (active.mode === 'tool') {
      handlers.drag(event);
      return;
    }
    const point = canvasPoint(canvas, event);
    handlers.pan(point.x - active.last.x, point.y - active.last.y);
    active.last = point;
  });

  function lift(event: PointerEvent) {
    touches.delete(event.pointerId);
    if (touches.size === 0) isPinching = false;
    if (active?.pointer !== event.pointerId) return;
    const { mode } = active;
    active = null;
    if (mode !== 'tool') return;
    if (event.type === 'pointerup') handlers.release(event);
    else handlers.cancel();
  }

  addEventListener('pointerup', lift);
  addEventListener('pointercancel', lift);

  canvas.addEventListener(
    'wheel',
    (event) => {
      event.preventDefault();
      handlers.zoom(canvasPoint(canvas, event), Math.exp(-event.deltaY * ZOOM_SPEED));
    },
    { passive: false }
  );

  return { cancel };
}
