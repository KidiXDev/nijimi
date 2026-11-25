import type { Editor } from '../pkg/nijimi.js';

const PROJECT_EXTENSION = '.nijimi';

async function decodeRgba(file: File) {
  const bitmap = await createImageBitmap(file);
  const { width, height } = bitmap;
  const context = new OffscreenCanvas(width, height).getContext('2d');
  if (!context) throw new Error('2D canvas is unavailable');
  context.drawImage(bitmap, 0, 0);
  bitmap.close();
  return context.getImageData(0, 0, width, height);
}

function download(blob: Blob, name: string) {
  const url = URL.createObjectURL(blob);
  const link = document.createElement('a');
  link.href = url;
  link.download = name;
  link.click();
  URL.revokeObjectURL(url);
}

export async function openFile(editor: Editor, file: File) {
  if (file.name.toLowerCase().endsWith(PROJECT_EXTENSION)) {
    await editor.loadProject(await file.bytes());
    return;
  }
  const { width, height, data } = await decodeRgba(file);
  editor.loadRgba(width, height, new Uint8Array(data.buffer));
}

export async function exportPng(editor: Editor) {
  const { width, height } = editor;
  const rgba = await editor.exportRgba();
  const output = new OffscreenCanvas(width, height);
  output
    .getContext('2d')
    ?.putImageData(new ImageData(new Uint8ClampedArray(rgba), width, height), 0, 0);
  download(await output.convertToBlob(), 'nijimi.png');
}

export async function saveProject(editor: Editor) {
  const file = await editor.saveProject();
  download(new Blob([new Uint8Array(file)]), `drawing${PROJECT_EXTENSION}`);
}
