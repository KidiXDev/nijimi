import type { BlendMode, Command, EditorState, LayerState } from '../pkg/nijimi.js';
import { element } from './dom';

type Position = { parent: LayerState | null; siblings: LayerState[]; index: number };

const list = element<HTMLUListElement>('layers');
const nameInput = element<HTMLInputElement>('layer-name');
const blendSelect = element<HTMLSelectElement>('layer-blend');
const opacityInput = element<HTMLInputElement>('layer-opacity');
const deleteButton = element<HTMLButtonElement>('delete-layer');
const upButton = element<HTMLButtonElement>('move-up');
const downButton = element<HTMLButtonElement>('move-down');

function locate(
  siblings: LayerState[],
  id: number | null,
  parent: LayerState | null = null
): Position | null {
  for (const [index, layer] of siblings.entries()) {
    const found =
      layer.id === id ? { parent, siblings, index } : locate(layer.children, id, layer);
    if (found) return found;
  }
  return null;
}

function moveUp(state: EditorState, { parent, siblings, index }: Position) {
  const above = siblings[index + 1];
  if (above?.kind === 'group') return { parent: above.id, index: 0 };
  if (above) return { parent: parent?.id ?? null, index: index + 1 };
  return parent && outOf(state, parent, 1);
}

function moveDown(state: EditorState, { parent, siblings, index }: Position) {
  const below = siblings[index - 1];
  if (below?.kind === 'group') return { parent: below.id, index: below.children.length };
  if (below) return { parent: parent?.id ?? null, index: index - 1 };
  return parent && outOf(state, parent, 0);
}

function outOf(state: EditorState, group: LayerState, offset: number) {
  const position = locate(state.layers, group.id);
  return position && {
    parent: position.parent?.id ?? null,
    index: position.index + offset
  };
}

export function createLayerPanel(
  readState: () => EditorState,
  dispatch: (command: Command) => void
) {
  function active() {
    const state = readState();
    const position = locate(state.layers, state.activeLayer);
    return position && { state, position, layer: position.siblings[position.index] };
  }

  function moveActive(destination: typeof moveUp) {
    const current = active();
    const target = current && destination(current.state, current.position);
    if (current && target) dispatch({ type: 'moveLayer', id: current.layer.id, ...target });
  }

  function layerRow(layer: LayerState, depth: number, activeLayer: number | null) {
    const row = document.createElement('li');
    row.style.setProperty('--depth', String(depth));
    const visible = document.createElement('input');
    visible.type = 'checkbox';
    visible.checked = layer.visible;
    visible.ariaLabel = `Show ${layer.name}`;
    visible.addEventListener('change', () =>
      dispatch({ type: 'setLayerVisible', id: layer.id, visible: visible.checked })
    );
    const select = document.createElement('button');
    select.type = 'button';
    select.textContent = layer.kind === 'group' ? `▾ ${layer.name}` : layer.name;
    select.ariaPressed = String(layer.id === activeLayer);
    select.addEventListener('click', () => dispatch({ type: 'selectLayer', id: layer.id }));
    row.append(visible, select);
    return row;
  }

  function layerRows(
    layers: LayerState[],
    activeLayer: number | null,
    depth = 0
  ): HTMLLIElement[] {
    return layers
      .toReversed()
      .flatMap((layer) => [
        layerRow(layer, depth, activeLayer),
        ...layerRows(layer.children, activeLayer, depth + 1)
      ]);
  }

  function update() {
    const current = active();
    const state = readState();
    list.replaceChildren(...layerRows(state.layers, state.activeLayer));
    for (const control of [
      nameInput,
      blendSelect,
      opacityInput,
      deleteButton,
      upButton,
      downButton
    ])
      control.disabled = !current;
    nameInput.value = current?.layer.name ?? '';
    blendSelect.value = current?.layer.blendMode ?? 'normal';
    opacityInput.valueAsNumber = current?.layer.opacity ?? 1;
  }

  function changeActive(event: string, control: HTMLElement, command: (id: number) => Command) {
    control.addEventListener(event, () => {
      const current = active();
      if (current) dispatch(command(current.layer.id));
    });
  }

  element('add-layer').addEventListener('click', () => dispatch({ type: 'addLayer' }));
  element('add-group').addEventListener('click', () => dispatch({ type: 'addGroup' }));
  upButton.addEventListener('click', () => moveActive(moveUp));
  downButton.addEventListener('click', () => moveActive(moveDown));
  changeActive('click', deleteButton, (id) => ({ type: 'deleteLayer', id }));
  changeActive('change', nameInput, (id) => ({
    type: 'renameLayer',
    id,
    name: nameInput.value
  }));
  changeActive('change', blendSelect, (id) => ({
    type: 'setLayerBlendMode',
    id,
    blendMode: blendSelect.value as BlendMode
  }));
  changeActive('change', opacityInput, (id) => ({
    type: 'setLayerOpacity',
    id,
    opacity: opacityInput.valueAsNumber
  }));

  return { update };
}
