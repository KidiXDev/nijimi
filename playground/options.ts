import type { Brush, Editor, SelectionMode } from '../pkg/nijimi.js';
import { element, fromHex, toHex } from './dom';

const bar = element('options');
const size = element<HTMLInputElement>('size');
const hardness = element<HTMLInputElement>('hardness');
const flow = element<HTMLInputElement>('flow');
const opacity = element<HTMLInputElement>('brush-opacity');
const color = element<HTMLInputElement>('color');
const blurRadius = element<HTMLInputElement>('blur-radius');
const tolerance = element<HTMLInputElement>('tolerance');
const contiguous = element<HTMLInputElement>('contiguous');
const selectionMode = element<HTMLSelectElement>('selection-mode');
const edgeRadius = element<HTMLInputElement>('edge-radius');

const SIZE_STEP = 1.15;

function showValue(input: HTMLInputElement) {
  const output = input.parentElement?.querySelector('output');
  if (!output) return;
  const isFraction = Number(input.max) <= 1;
  output.value = isFraction ? `${Math.round(input.valueAsNumber * 100)}%` : input.value;
}

function changed(input: HTMLInputElement) {
  input.dispatchEvent(new Event('input'));
}

export const options = {
  color: () => fromHex(color.value),
  brushSize: () => size.valueAsNumber,
  blurRadius: () => blurRadius.valueAsNumber,
  tolerance: () => tolerance.valueAsNumber,
  contiguous: () => contiguous.checked,
  edgeRadius: () => edgeRadius.valueAsNumber,
  selectionMode({ shiftKey, altKey }: PointerEvent): SelectionMode {
    if (shiftKey && altKey) return 'intersect';
    if (shiftKey) return 'add';
    if (altKey) return 'subtract';
    return selectionMode.value as SelectionMode;
  },
  brush: (): Brush => ({
    diameter: size.valueAsNumber,
    hardness: hardness.valueAsNumber,
    flow: flow.valueAsNumber,
    opacity: opacity.valueAsNumber,
    color: options.color()
  }),
  pickColor(bytes: Uint8Array) {
    color.value = toHex([...bytes.slice(0, 3)].map((byte) => byte / 255));
    changed(color);
  },
  resizeBrush(direction: 1 | -1) {
    const next = size.valueAsNumber * SIZE_STEP ** direction;
    size.valueAsNumber = direction > 0 ? Math.ceil(next) : Math.floor(next);
    changed(size);
  },
  showFor(tool: string) {
    for (const group of bar.querySelectorAll<HTMLElement>('[data-tools]'))
      group.hidden = !group.dataset.tools?.split(' ').includes(tool);
  }
};

export function bindOptions(editor: Editor, attempt: (work: () => void) => boolean) {
  const apply = () => attempt(() => editor.setBrush(options.brush()));
  for (const input of [size, hardness, flow, opacity, color]) input.addEventListener('input', apply);
  for (const input of bar.querySelectorAll<HTMLInputElement>('input[type=range]')) {
    input.addEventListener('input', () => showValue(input));
    showValue(input);
  }
  apply();
}
