import type { Editor, Point } from '../pkg/nijimi.js';
import { svgElement } from './dom';

const SOURCE_ARM = 8;

export function createOverlay(svg: Element, editor: Editor) {
  const outline = svgElement('path', { class: 'drag-outline' });
  const source = svgElement('path', { class: 'copy-source' });
  const cursor = svgElement('circle', { class: 'brush-cursor', r: '0' });
  svg.append(outline, source, cursor);

  function toScreen({ x, y }: Point): Point {
    const origin = editor.documentPoint(0, 0);
    const scale = editor.zoom / devicePixelRatio;
    return { x: (x - origin.x) * scale, y: (y - origin.y) * scale };
  }

  function showPolygon(points: Point[]) {
    const corners = points.map(toScreen).map(({ x, y }) => `${x} ${y}`);
    outline.setAttribute('d', corners.length ? `M${corners.join('L')}Z` : '');
  }

  function showEllipse(start: Point, end: Point) {
    const [a, b] = [toScreen(start), toScreen(end)];
    const [rx, ry] = [Math.abs(b.x - a.x) / 2, Math.abs(b.y - a.y) / 2];
    const arc = (dx: number) => `a${rx} ${ry} 0 1 0 ${dx} 0`;
    const middle = (a.y + b.y) / 2;
    outline.setAttribute(
      'd',
      `M${Math.min(a.x, b.x)} ${middle}${arc(rx * 2)}${arc(-rx * 2)}Z`
    );
  }

  function showSource(point: Point | null) {
    const { x, y } = point ? toScreen(point) : { x: 0, y: 0 };
    const cross = `M${x - SOURCE_ARM} ${y}h${SOURCE_ARM * 2}M${x} ${y - SOURCE_ARM}v${SOURCE_ARM * 2}`;
    source.setAttribute('d', point ? cross : '');
  }

  function showCursor(position: Point | null, diameter: number) {
    const radius = position ? (diameter * editor.zoom) / devicePixelRatio / 2 : 0;
    cursor.setAttribute('r', String(radius));
    cursor.setAttribute('cx', String(position?.x ?? 0));
    cursor.setAttribute('cy', String(position?.y ?? 0));
  }

  return { showPolygon, showEllipse, showSource, showCursor };
}

export type Overlay = ReturnType<typeof createOverlay>;
