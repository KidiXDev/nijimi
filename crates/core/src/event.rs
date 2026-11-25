use serde::Serialize;

use crate::LayerId;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Event {
    DocumentReplaced,
    CanvasResized { width: u32, height: u32 },
    PreviewChanged,
    LayerAdded { id: LayerId },
    LayerRemoved { id: LayerId },
    LayerMoved { id: LayerId },
    LayerChanged { id: LayerId },
    PixelsChanged { id: LayerId },
    MaskChanged { id: LayerId },
    ActiveLayerChanged { id: Option<LayerId> },
    HistoryChanged { can_undo: bool, can_redo: bool },
    DeviceLost { message: String },
    GpuError { message: String },
}
