use std::error::Error;
use std::fmt::{Display, Formatter};

use glaphica_core::BrushId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrushRegistryError {
    BrushIdOutOfRange {
        brush_id: BrushId,
        max_brushes: usize,
    },
    BrushAlreadyRegistered {
        brush_id: BrushId,
    },
    BrushNotRegistered {
        brush_id: BrushId,
    },
}

impl Display for BrushRegistryError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BrushIdOutOfRange {
                brush_id,
                max_brushes,
            } => write!(
                f,
                "brush id {} is out of range, max registered brushes is {}",
                brush_id.0, max_brushes
            ),
            Self::BrushAlreadyRegistered { brush_id } => {
                write!(f, "brush id {} is already registered", brush_id.0)
            }
            Self::BrushNotRegistered { brush_id } => {
                write!(f, "brush id {} is not registered", brush_id.0)
            }
        }
    }
}

impl Error for BrushRegistryError {}

pub struct BrushRegistry<T> {
    slots: Vec<Option<T>>,
}

impl<T> BrushRegistry<T> {
    pub fn with_max_brushes(max_brushes: usize) -> Self {
        let mut slots = Vec::with_capacity(max_brushes);
        slots.resize_with(max_brushes, || None);
        Self { slots }
    }

    pub fn register(&mut self, brush_id: BrushId, pipeline: T) -> Result<(), BrushRegistryError> {
        self.ensure_can_register(brush_id)?;
        let max_brushes = self.slots.len();
        let index = self.brush_index(brush_id)?;
        let Some(slot) = self.slots.get_mut(index) else {
            return Err(BrushRegistryError::BrushIdOutOfRange {
                brush_id,
                max_brushes,
            });
        };
        *slot = Some(pipeline);
        Ok(())
    }

    pub fn ensure_can_register(&self, brush_id: BrushId) -> Result<(), BrushRegistryError> {
        let index = self.brush_index(brush_id)?;
        let Some(slot) = self.slots.get(index) else {
            return Err(BrushRegistryError::BrushIdOutOfRange {
                brush_id,
                max_brushes: self.slots.len(),
            });
        };
        if slot.is_some() {
            return Err(BrushRegistryError::BrushAlreadyRegistered { brush_id });
        }
        Ok(())
    }

    pub fn get_mut(&mut self, brush_id: BrushId) -> Result<&mut T, BrushRegistryError> {
        let max_brushes = self.slots.len();
        let index = self.brush_index(brush_id)?;
        let Some(slot) = self.slots.get_mut(index) else {
            return Err(BrushRegistryError::BrushIdOutOfRange {
                brush_id,
                max_brushes,
            });
        };
        let Some(value) = slot.as_mut() else {
            return Err(BrushRegistryError::BrushNotRegistered { brush_id });
        };
        Ok(value)
    }

    pub fn get(&self, brush_id: BrushId) -> Result<&T, BrushRegistryError> {
        let index = self.brush_index(brush_id)?;
        let Some(slot) = self.slots.get(index) else {
            return Err(BrushRegistryError::BrushIdOutOfRange {
                brush_id,
                max_brushes: self.slots.len(),
            });
        };
        let Some(value) = slot.as_ref() else {
            return Err(BrushRegistryError::BrushNotRegistered { brush_id });
        };
        Ok(value)
    }

    fn brush_index(&self, brush_id: BrushId) -> Result<usize, BrushRegistryError> {
        let index =
            usize::try_from(brush_id.0).map_err(|_| BrushRegistryError::BrushIdOutOfRange {
                brush_id,
                max_brushes: self.slots.len(),
            })?;
        if index >= self.slots.len() {
            return Err(BrushRegistryError::BrushIdOutOfRange {
                brush_id,
                max_brushes: self.slots.len(),
            });
        }
        Ok(index)
    }
}

#[cfg(test)]
mod tests {
    use super::{BrushRegistry, BrushRegistryError};
    use glaphica_core::BrushId;

    #[test]
    fn register_and_lookup_by_brush_id_index() {
        let mut registry = BrushRegistry::with_max_brushes(4);
        let register = registry.register(BrushId(2), 9u32);
        assert!(register.is_ok());

        let value = registry.get_mut(BrushId(2));
        assert_eq!(value.map(|v| *v), Ok(9));
    }

    #[test]
    fn register_rejects_duplicate_brush_id() {
        let mut registry = BrushRegistry::with_max_brushes(2);
        assert!(registry.register(BrushId(1), 7u32).is_ok());

        let err = registry.register(BrushId(1), 8u32);
        assert_eq!(
            err,
            Err(BrushRegistryError::BrushAlreadyRegistered {
                brush_id: BrushId(1)
            })
        );
    }

    #[test]
    fn register_rejects_out_of_range_brush_id() {
        let mut registry = BrushRegistry::with_max_brushes(1);
        let err = registry.register(BrushId(3), 1u32);
        assert_eq!(
            err,
            Err(BrushRegistryError::BrushIdOutOfRange {
                brush_id: BrushId(3),
                max_brushes: 1
            })
        );
    }

    #[test]
    fn get_mut_rejects_unregistered_brush_id() {
        let mut registry: BrushRegistry<u32> = BrushRegistry::with_max_brushes(4);
        let err = registry.get_mut(BrushId(0));
        assert_eq!(
            err,
            Err(BrushRegistryError::BrushNotRegistered {
                brush_id: BrushId(0)
            })
        );
    }
}
