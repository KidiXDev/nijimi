import { ICONS, type IconName } from './icons';

export function element<T extends HTMLElement>(id: string) {
  const found = document.getElementById(id);
  if (!found) throw new Error(`Missing #${id}`);
  return found as T;
}

export function svgElement(name: string, attributes: Record<string, string>) {
  const created = document.createElementNS('http://www.w3.org/2000/svg', name);
  for (const [key, value] of Object.entries(attributes)) created.setAttribute(key, value);
  return created;
}

export function icon(name: IconName) {
  const svg = svgElement('svg', { viewBox: '0 0 24 24', class: 'icon', 'aria-hidden': 'true' });
  svg.innerHTML = ICONS[name];
  return svg;
}

export function fillIcons() {
  for (const host of document.querySelectorAll<HTMLElement>('[data-icon]'))
    host.prepend(icon(host.dataset.icon as IconName));
}

export function toHex(channels: number[]) {
  const digits = channels.map((value) =>
    Math.round(value * 255)
      .toString(16)
      .padStart(2, '0')
  );
  return `#${digits.join('')}`;
}

export function fromHex(color: string): [number, number, number] {
  const channel = (start: number) => parseInt(color.slice(start, start + 2), 16) / 255;
  return [channel(1), channel(3), channel(5)];
}
