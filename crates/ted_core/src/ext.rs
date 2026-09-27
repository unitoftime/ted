//! Type-keyed storage, used for editor-wide and buffer-local state.

use std::any::{Any, TypeId};
use std::collections::HashMap;

/// Type-keyed storage: at most one value per type.
#[derive(Default)]
pub struct Extensions {
    map: HashMap<TypeId, Box<dyn Any>>,
}

impl Extensions {
    pub fn get<T: 'static>(&self) -> Option<&T> {
        self.map.get(&TypeId::of::<T>()).and_then(|b| b.downcast_ref())
    }

    /// Stores `value`, replacing any `T` already there.
    pub fn insert<T: 'static>(&mut self, value: T) {
        self.map.insert(TypeId::of::<T>(), Box::new(value));
    }

    /// The stored `T`, created with `Default` on first use.
    pub fn get_mut<T: Default + 'static>(&mut self) -> &mut T {
        self.map
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Box::new(T::default()))
            .downcast_mut()
            .expect("extension slot holds its own type")
    }
}
