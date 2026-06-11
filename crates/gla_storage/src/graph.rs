use crate::GlobalStorageError;
use gla_color::GlaFormat;
use gla_image::GlaImageLayout;
use gla_ir::{ImageId, ImageRole};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ImageSpec {
    pub(crate) format: GlaFormat,
    pub(crate) layout: GlaImageLayout,
    pub(crate) role: ImageRole,
}

pub(crate) fn validate_specs(
    specs: &HashMap<ImageId, ImageSpec>,
) -> Result<(), GlobalStorageError> {
    for (id, spec) in specs {
        let ImageRole::Derived(command) = &spec.role else {
            continue;
        };
        for read in &command.reads {
            if read.image == *id {
                return Err(GlobalStorageError::RegistryCommandReadsDestination { dst: *id });
            }
            if !specs.contains_key(&read.image) {
                return Err(GlobalStorageError::MissingImage { id: read.image });
            }
        }
    }

    let mut visited = HashSet::new();
    let mut visiting = HashSet::new();
    for id in specs.keys().copied() {
        visit_spec(id, specs, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn visit_spec(
    id: ImageId,
    specs: &HashMap<ImageId, ImageSpec>,
    visiting: &mut HashSet<ImageId>,
    visited: &mut HashSet<ImageId>,
) -> Result<(), GlobalStorageError> {
    if visited.contains(&id) {
        return Ok(());
    }
    if !visiting.insert(id) {
        return Err(GlobalStorageError::RegistryCycle { id });
    }

    if let Some(ImageSpec {
        role: ImageRole::Derived(command),
        ..
    }) = specs.get(&id)
    {
        for read in &command.reads {
            visit_spec(read.image, specs, visiting, visited)?;
        }
    }

    visiting.remove(&id);
    visited.insert(id);
    Ok(())
}
