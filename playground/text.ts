import type { Editor, LayerState, Point, TextSettings } from '../pkg/nijimi.js';
import { element, fromHex, toHex } from './dom';
import { options } from './options';

const panel = element<HTMLFieldSetElement>('text-panel');
const content = element<HTMLTextAreaElement>('text-content');
const font = element<HTMLSelectElement>('text-font');
const size = element<HTMLInputElement>('text-size');
const bold = element<HTMLInputElement>('text-bold');
const italic = element<HTMLInputElement>('text-italic');
const align = element<HTMLSelectElement>('text-align');
const color = element<HTMLInputElement>('text-color');
const rasterize = element<HTMLButtonElement>('rasterize');

const HEIGHT_RATIO = 0.06;
const SMALLEST_SIZE = 12;
const BOLD = 700;
const REGULAR = 400;

export function createTextPanel(
  editor: Editor,
  activeLayer: () => LayerState | null,
  attempt: (work: () => void) => boolean
) {
  let isTyping = false;

  function edited(text: TextSettings): TextSettings {
    return {
      ...text,
      text: content.value,
      fontFamily: font.value,
      fontSize: size.valueAsNumber,
      weight: bold.checked ? BOLD : REGULAR,
      italic: italic.checked,
      align: align.value as TextSettings['align'],
      color: fromHex(color.value)
    };
  }

  function apply() {
    const layer = activeLayer();
    const text = layer?.text;
    if (!layer || !text) return false;
    return attempt(() => editor.setText(layer.id, edited(text)));
  }

  function type() {
    if (isTyping) editor.dispatch({ type: 'undo' });
    isTyping = apply();
  }

  content.addEventListener('input', type);
  content.addEventListener('blur', () => (isTyping = false));
  for (const input of [font, size, bold, italic, align, color])
    input.addEventListener('change', apply);
  rasterize.addEventListener('click', () => {
    const layer = activeLayer();
    if (layer) attempt(() => editor.dispatch({ type: 'rasterizeText', id: layer.id }));
  });

  return {
    update() {
      const text = activeLayer()?.text;
      panel.hidden = !text;
      if (!text || document.activeElement === content) return;
      content.value = text.text;
      const isListed = [...font.options].some((option) => option.value === text.fontFamily);
      if (!isListed) font.add(new Option(text.fontFamily, text.fontFamily));
      font.value = text.fontFamily;
      size.valueAsNumber = text.fontSize;
      bold.checked = text.weight >= BOLD;
      italic.checked = text.italic;
      align.value = text.align;
      color.value = toHex(text.color);
    },
    add({ x, y }: Point) {
      const fontSize = Math.max(SMALLEST_SIZE, Math.round(editor.height * HEIGHT_RATIO));
      const fontFamily = font.options[0].value;
      const settings = { x, y, text: 'Text', fontFamily, fontSize, color: options.color() };
      if (!attempt(() => editor.addText(settings))) return;
      this.update();
      content.focus();
      content.select();
    }
  };
}
