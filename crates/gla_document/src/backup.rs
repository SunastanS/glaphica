use atlas::{AtlasError, Backend, BackendId, CachedTileGroup};

#[derive(Debug)]
pub struct DocumentBackupStore {
    backend: Backend,
    backend_id: BackendId,
    retained_groups: Vec<CachedTileGroup>,
}

impl DocumentBackupStore {
    pub fn new(backend: Backend) -> Result<Self, AtlasError> {
        let backend_id = backend.backend_id()?;
        Ok(Self {
            backend,
            backend_id,
            retained_groups: Vec::new(),
        })
    }

    pub fn backend(&self) -> &Backend {
        &self.backend
    }

    pub fn backend_id(&self) -> BackendId {
        self.backend_id
    }

    pub fn retained_groups(&self) -> &[CachedTileGroup] {
        &self.retained_groups
    }

    pub fn retain_cached_group(
        &mut self,
        non_empty_tile_count: usize,
    ) -> Result<CachedTileGroup, AtlasError> {
        let cached_group = if non_empty_tile_count == 0 {
            self.backend.create_cached_group()?
        } else {
            self.backend.alloc_cached(non_empty_tile_count)?
        };
        self.retained_groups.push(cached_group.clone());
        Ok(cached_group)
    }
}
