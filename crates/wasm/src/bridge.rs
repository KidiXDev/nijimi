use js_sys::JSON;
use nijimi_core::{Document, Layer, LayerContent, LayerId, LayerProperties};
use serde::{Serialize, de::DeserializeOwned};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(typescript_custom_section)]
const TYPES: &str = r#"
export type BlendMode =
  | 'normal'
  | 'multiply'
  | 'screen'
  | 'overlay'
  | 'softLight'
  | 'add'
  | 'darken'
  | 'lighten';

/** Diameter is in document pixels. Every other value is between 0 and 1. */
export interface Brush {
  diameter: number;
  hardness: number;
  flow: number;
  opacity: number;
  color: [red: number, green: number, blue: number];
}

export interface StrokePoint {
  x: number;
  y: number;
  pressure?: number;
}

/**
 * Layers are ordered from bottom to top, so index 0 is the bottom.
 * A new layer without `parent` and `index` goes above the active layer.
 * `parent: null` is the top level.
 */
export type Command =
  | { type: 'addLayer'; name?: string; parent?: number | null; index?: number }
  | { type: 'addGroup'; name?: string; parent?: number | null; index?: number }
  | { type: 'deleteLayer'; id: number }
  | { type: 'moveLayer'; id: number; parent: number | null; index: number }
  | { type: 'renameLayer'; id: number; name: string }
  | { type: 'setLayerVisible'; id: number; visible: boolean }
  | { type: 'setLayerOpacity'; id: number; opacity: number }
  | { type: 'setLayerBlendMode'; id: number; blendMode: BlendMode }
  | { type: 'selectLayer'; id: number }
  | { type: 'paintStroke'; layer: number; brush: Brush; points: StrokePoint[] }
  | { type: 'undo' }
  | { type: 'redo' };

export type EditorEvent =
  | { type: 'documentReplaced' }
  | { type: 'layerAdded'; id: number }
  | { type: 'layerRemoved'; id: number }
  | { type: 'layerMoved'; id: number }
  | { type: 'layerChanged'; id: number }
  | { type: 'pixelsChanged'; id: number }
  | { type: 'activeLayerChanged'; id: number | null }
  | { type: 'historyChanged'; canUndo: boolean; canRedo: boolean }
  | { type: 'deviceLost'; message: string }
  | { type: 'gpuError'; message: string };

export interface LayerState {
  id: number;
  name: string;
  visible: boolean;
  opacity: number;
  blendMode: BlendMode;
  kind: 'raster' | 'group';
  children: LayerState[];
}

export interface EditorState {
  width: number;
  height: number;
  activeLayer: number | null;
  canUndo: boolean;
  canRedo: boolean;
  layers: LayerState[];
}
"#;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorState<'a> {
    width: u32,
    height: u32,
    active_layer: Option<LayerId>,
    can_undo: bool,
    can_redo: bool,
    layers: Vec<LayerState<'a>>,
}

#[derive(Serialize)]
struct LayerState<'a> {
    id: LayerId,
    #[serde(flatten)]
    properties: &'a LayerProperties,
    kind: &'static str,
    children: Vec<LayerState<'a>>,
}

impl<'a> EditorState<'a> {
    pub fn new<T: Clone>(document: &'a Document<T>) -> Self {
        let (width, height) = document.size();
        Self {
            width,
            height,
            active_layer: document.active(),
            can_undo: document.can_undo(),
            can_redo: document.can_redo(),
            layers: layer_states(document.layers()),
        }
    }
}

fn layer_states<T>(layers: &[Layer<T>]) -> Vec<LayerState<'_>> {
    layers
        .iter()
        .map(|layer| LayerState {
            id: layer.id,
            properties: &layer.properties,
            kind: match layer.content {
                LayerContent::Raster(_) => "raster",
                LayerContent::Group(_) => "group",
            },
            children: layer_states(layer.children()),
        })
        .collect()
}

pub fn to_js(value: &impl Serialize) -> Result<JsValue, JsValue> {
    let json = serde_json::to_string(value).map_err(JsError::from)?;
    JSON::parse(&json)
}

pub fn from_js<T: DeserializeOwned>(value: &JsValue) -> Result<T, JsValue> {
    let json = JSON::stringify(value)?
        .as_string()
        .ok_or_else(|| JsError::new("Expected an object."))?;
    Ok(serde_json::from_str(&json).map_err(JsError::from)?)
}
