use nijimi_core::{
    BlendMode, Document, DocumentError, Event, Layer, LayerId, LayerPlace, MAX_GROUP_DEPTH,
    MAX_LAYERS, TILE_BYTES, TileCoord,
};

type TestDocument = Document<u32>;

const SIZE: (u32, u32) = (1000, 600);
const TILE: u64 = TILE_BYTES as u64;

fn coord(x: u32, y: u32) -> TileCoord {
    TileCoord::new(x, y)
}

fn top_of(parent: Option<LayerId>) -> Option<LayerPlace> {
    Some(LayerPlace {
        parent,
        index: usize::MAX,
    })
}

fn bottom_of(parent: Option<LayerId>) -> Option<LayerPlace> {
    Some(LayerPlace { parent, index: 0 })
}

fn sample() -> TestDocument {
    let mut document = TestDocument::new(SIZE);
    let bottom = document.add_layer(None, None).unwrap();
    let group = document.add_group(None, None).unwrap();
    document.add_layer(None, top_of(Some(group))).unwrap();
    document
        .replace_tiles(
            bottom,
            vec![(coord(0, 0), Some(10)), (coord(1, 1), Some(11))],
        )
        .unwrap();
    document.take_events();
    document.take_dirty();
    document
}

const BOTTOM: LayerId = 1;
const GROUP: LayerId = 2;
const CHILD: LayerId = 3;

fn state(document: &TestDocument) -> (Vec<Layer<u32>>, Option<LayerId>) {
    (document.layers().to_vec(), document.active())
}

fn assert_undoable(name: &str, change: impl FnOnce(&mut TestDocument)) {
    let mut document = sample();
    let before = state(&document);
    change(&mut document);
    let after = state(&document);
    assert_ne!(before, after, "{name} changed nothing");
    let dirty = document.take_dirty();
    assert!(document.undo(), "{name} recorded nothing");
    assert_eq!(state(&document), before, "{name} was not undone");
    assert_eq!(
        document.take_dirty(),
        dirty,
        "{name} undo redraws other tiles"
    );
    assert!(document.redo());
    assert_eq!(state(&document), after, "{name} was not redone");
    assert!(document.undo());
    assert_eq!(state(&document), before);
}

#[test]
fn every_document_change_can_be_undone_and_redone() {
    assert_undoable("add layer", |document| {
        document
            .add_layer(Some("New".into()), bottom_of(None))
            .unwrap();
    });
    assert_undoable("add group", |document| {
        document.add_group(None, bottom_of(Some(GROUP))).unwrap();
    });
    assert_undoable("delete layer", |document| {
        document.delete_layer(BOTTOM).unwrap();
    });
    assert_undoable("delete group with the active layer", |document| {
        document.delete_layer(GROUP).unwrap();
    });
    assert_undoable("reorder", |document| {
        document.move_layer(BOTTOM, None, 1).unwrap();
    });
    assert_undoable("move into group", |document| {
        document.move_layer(BOTTOM, Some(GROUP), 0).unwrap();
    });
    assert_undoable("move out of group", |document| {
        document.move_layer(CHILD, None, 0).unwrap();
    });
    assert_undoable("rename", |document| {
        document.rename_layer(BOTTOM, "Renamed".into()).unwrap();
    });
    assert_undoable("visibility", |document| {
        document.set_layer_visible(BOTTOM, false).unwrap();
    });
    assert_undoable("opacity", |document| {
        document.set_layer_opacity(BOTTOM, 0.25).unwrap();
    });
    assert_undoable("blend mode", |document| {
        document
            .set_layer_blend_mode(BOTTOM, BlendMode::Multiply)
            .unwrap();
    });
    assert_undoable("tiles", |document| {
        document
            .replace_tiles(
                BOTTOM,
                vec![
                    (coord(0, 0), Some(20)),
                    (coord(1, 1), None),
                    (coord(2, 2), Some(21)),
                ],
            )
            .unwrap();
    });
}

#[test]
fn a_long_mixed_history_unwinds_completely() {
    let mut document = TestDocument::new(SIZE);
    let mut states = vec![state(&document)];
    let steps: [fn(&mut TestDocument); 7] = [
        |document| {
            document.add_layer(None, None).unwrap();
        },
        |document| {
            document
                .replace_tiles(1, vec![(coord(0, 0), Some(1))])
                .unwrap();
        },
        |document| {
            document.add_group(None, None).unwrap();
        },
        |document| {
            document.move_layer(1, Some(2), 0).unwrap();
        },
        |document| {
            document.set_layer_opacity(2, 0.5).unwrap();
        },
        |document| {
            document.delete_layer(2).unwrap();
        },
        |document| {
            document.add_layer(None, None).unwrap();
        },
    ];
    for step in steps {
        step(&mut document);
        states.push(state(&document));
    }
    for expected in states.iter().rev().skip(1) {
        assert!(document.undo());
        assert_eq!(&state(&document), expected);
    }
    assert!(!document.undo());
    for expected in states.iter().skip(1) {
        assert!(document.redo());
        assert_eq!(&state(&document), expected);
    }
    assert!(!document.redo());
}

#[test]
fn changes_that_do_nothing_are_not_recorded() {
    let mut document = sample();
    document.rename_layer(BOTTOM, "Layer 1".into()).unwrap();
    document.set_layer_visible(BOTTOM, true).unwrap();
    document.move_layer(BOTTOM, None, 0).unwrap();
    document.replace_tiles(BOTTOM, Vec::new()).unwrap();
    assert!(document.take_events().is_empty());
}

#[test]
fn a_new_change_clears_redo() {
    let mut document = sample();
    document.undo();
    assert!(document.can_redo());
    document.add_layer(None, None).unwrap();
    assert!(!document.can_redo());
}

#[test]
fn new_layers_go_above_the_active_layer() {
    let mut document = sample();
    document.select_layer(BOTTOM).unwrap();
    let added = document.add_layer(None, None).unwrap();
    let order: Vec<_> = document.layers().iter().map(|layer| layer.id).collect();
    assert_eq!(order, [BOTTOM, added, GROUP]);
    assert_eq!(document.active(), Some(added));
}

#[test]
fn deleting_the_active_layer_selects_a_neighbor() {
    let mut document = sample();
    document.delete_layer(CHILD).unwrap();
    assert_eq!(document.active(), Some(GROUP));
    document.delete_layer(GROUP).unwrap();
    assert_eq!(document.active(), Some(BOTTOM));
    document.delete_layer(BOTTOM).unwrap();
    assert_eq!(document.active(), None);
}

#[test]
fn invalid_changes_are_rejected_and_leave_the_document_alone() {
    let mut document = sample();
    let before = state(&document);
    use DocumentError::*;
    assert_eq!(document.delete_layer(99), Err(LayerNotFound(99)));
    assert_eq!(document.select_layer(99), Err(LayerNotFound(99)));
    assert_eq!(
        document.add_layer(None, top_of(Some(BOTTOM))),
        Err(NotAGroup(BOTTOM))
    );
    assert_eq!(
        document.move_layer(BOTTOM, Some(99), 0),
        Err(LayerNotFound(99))
    );
    assert_eq!(
        document.move_layer(GROUP, Some(GROUP), 0),
        Err(MoveIntoItself)
    );
    for opacity in [-0.1, 1.1, f32::NAN, f32::INFINITY] {
        assert_eq!(
            document.set_layer_opacity(BOTTOM, opacity),
            Err(InvalidOpacity)
        );
    }
    assert_eq!(
        document.rename_layer(BOTTOM, "a".repeat(257)),
        Err(NameTooLong)
    );
    assert_eq!(
        document.replace_tiles(GROUP, vec![(coord(0, 0), Some(1))]),
        Err(NotARasterLayer(GROUP))
    );
    assert_eq!(
        document.replace_tiles(BOTTOM, vec![(coord(4, 0), Some(1))]),
        Err(TileOutOfBounds(coord(4, 0)))
    );
    assert_eq!(state(&document), before);
    assert!(document.take_events().is_empty());
}

#[test]
fn layer_count_and_depth_are_limited() {
    let mut document = TestDocument::new(SIZE);
    let mut parent = None;
    for _ in 0..MAX_GROUP_DEPTH {
        parent = Some(document.add_group(None, top_of(parent)).unwrap());
    }
    assert_eq!(
        document.add_group(None, top_of(parent)),
        Err(DocumentError::TooDeep)
    );
    document.add_layer(None, top_of(parent)).unwrap();
    let loose = document.add_group(None, bottom_of(None)).unwrap();
    assert_eq!(
        document.move_layer(loose, parent, 0),
        Err(DocumentError::TooDeep)
    );
    for _ in MAX_GROUP_DEPTH + 2..MAX_LAYERS {
        document.add_layer(None, bottom_of(None)).unwrap();
    }
    assert_eq!(
        document.add_layer(None, None),
        Err(DocumentError::TooManyLayers)
    );
    let layers = document.layers().to_vec();
    assert!(TestDocument::with_layers(SIZE, layers, None).is_ok());
}

#[test]
fn history_keeps_within_its_memory_limit() {
    let mut document = sample();
    document.set_history_limit(3 * TILE);
    for value in 0..10 {
        document
            .replace_tiles(BOTTOM, vec![(coord(0, 0), Some(value))])
            .unwrap();
        assert!(document.history_bytes() <= 3 * TILE);
    }
    let mut undone = 0;
    while document.undo() {
        undone += 1;
        assert!(document.history_bytes() <= 3 * TILE);
    }
    assert_eq!(undone, 2);
    document.set_history_limit(0);
    assert!(!document.can_redo());
    assert_eq!(document.history_bytes(), 0);
}

#[test]
fn history_counts_the_tiles_of_deleted_layers() {
    let mut document = sample();
    let before = document.history_bytes();
    document.delete_layer(BOTTOM).unwrap();
    assert!(document.history_bytes() >= before + 2 * TILE);
}

#[test]
fn only_affected_tiles_are_redrawn() {
    let mut document = sample();
    document.rename_layer(BOTTOM, "Renamed".into()).unwrap();
    assert!(document.take_dirty().is_empty());
    document.set_layer_opacity(BOTTOM, 0.5).unwrap();
    assert_eq!(
        document.take_dirty().into_iter().collect::<Vec<_>>(),
        [coord(0, 0), coord(1, 1)]
    );
}

#[test]
fn events_describe_each_change() {
    let mut document = TestDocument::new(SIZE);
    document.add_layer(None, None).unwrap();
    assert_eq!(
        document.take_events(),
        [
            Event::LayerAdded { id: 1 },
            Event::ActiveLayerChanged { id: Some(1) },
            Event::HistoryChanged {
                can_undo: true,
                can_redo: false
            },
        ]
    );
    document.undo();
    assert_eq!(
        document.take_events(),
        [
            Event::LayerRemoved { id: 1 },
            Event::ActiveLayerChanged { id: None },
            Event::HistoryChanged {
                can_undo: false,
                can_redo: true
            },
        ]
    );
}

#[test]
fn the_latest_change_stays_undoable_when_it_exceeds_the_limit() {
    let mut document = sample();
    document.set_history_limit(TILE);
    let before = state(&document);
    document.delete_layer(BOTTOM).unwrap();
    assert!(document.history_bytes() > TILE);
    assert!(document.undo());
    assert_eq!(state(&document), before);
    assert!(document.redo());
    document.rename_layer(GROUP, "Renamed".into()).unwrap();
    assert!(document.history_bytes() <= TILE);
}

#[test]
fn resizing_the_canvas_redraws_every_tile() {
    let mut document = sample();
    document.replace_rasters((256, 256), Vec::new()).unwrap();
    document.replace_rasters(SIZE, Vec::new()).unwrap();
    assert!(document.take_dirty().into_iter().eq({
        let mut all: Vec<_> = TileCoord::all(SIZE).collect();
        all.sort();
        all
    }));
}
