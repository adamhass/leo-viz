//! A small mutable palette shared by cached globe/map geometry and CPU drawing.
//! Geometry stays uploaded when replay time changes an AOI's status.
use std::sync::RwLock;

pub(super) struct Colors {
    pub state: RwLock<State>,
}

pub(super) struct State {
    pub revision: u64,
    pub rgba: Vec<[u8; 4]>,
}

impl Colors {
    pub fn new(rgb: impl Iterator<Item = [u8; 3]>) -> Self {
        Self {
            state: RwLock::new(State {
                revision: 0,
                rgba: rgb.map(|[r, g, b]| [r, g, b, 255]).collect(),
            }),
        }
    }

    pub fn update(&self, rgb: &[[u8; 3]]) {
        let mut state = self.state.write().unwrap();
        assert_eq!(state.rgba.len(), rgb.len());
        let mut changed = false;
        for (old, &[r, g, b]) in state.rgba.iter_mut().zip(rgb) {
            let next = [r, g, b, 255];
            changed |= *old != next;
            *old = next;
        }
        if changed {
            state.revision += 1;
        }
    }
}
