use gla_image::GlaImageKey;
use gla_ir::*;
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};

/// Re-exported from gla_ir.
pub use gla_ir::ImageRole;

#[derive(Debug)]
pub enum DocError {
    EmptyRegistry,
    MissingRoot { root: ImageId },
    MissingImage { id: ImageId },
    UnreachableImage { id: ImageId },
    RegistryCommandReadsDestination { dst: ImageId },
    RegistryCycle { id: ImageId },
    BindingMissing { id: ImageId },
    BindingExtra { id: ImageId },
}

impl Display for DocError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyRegistry => f.write_str("registry graph is empty"),
            Self::MissingRoot { root } => write!(f, "registry root {root:?} is not declared"),
            Self::MissingImage { id } => write!(f, "image {id:?} is not declared"),
            Self::UnreachableImage { id } => {
                write!(f, "image {id:?} is not reachable from the registry root")
            }
            Self::RegistryCommandReadsDestination { dst } => {
                write!(f, "registry command for {dst:?} reads its destination")
            }
            Self::RegistryCycle { id } => {
                write!(f, "registry graph has a dependency cycle at {id:?}")
            }
            Self::BindingMissing { id } => write!(f, "binding table is missing {id:?}"),
            Self::BindingExtra { id } => write!(f, "binding table has extra image {id:?}"),
        }
    }
}

#[derive(Debug)]
pub struct Document {
    root: ImageId,
    roles: HashMap<ImageId, ImageRole>,
    bindings: HashMap<ImageId, GlaImageKey>,
    version: DocumentVersionId,
}

impl Document {
    pub fn new(
        root: ImageId,
        roles: HashMap<ImageId, ImageRole>,
        bindings: HashMap<ImageId, GlaImageKey>,
    ) -> Result<Self, DocError> {
        validate_document(root, &roles)?;
        for id in roles.keys() {
            if !bindings.contains_key(id) {
                return Err(DocError::BindingMissing { id: *id });
            }
        }
        for id in bindings.keys() {
            if !roles.contains_key(id) {
                return Err(DocError::BindingExtra { id: *id });
            }
        }
        Ok(Self {
            root,
            roles,
            bindings,
            version: DocumentVersionId::default(),
        })
    }

    pub fn root(&self) -> ImageId {
        self.root
    }

    pub fn roles(&self) -> &HashMap<ImageId, ImageRole> {
        &self.roles
    }

    pub fn role(&self, id: ImageId) -> Option<&ImageRole> {
        self.roles.get(&id)
    }

    pub fn bindings(&self) -> &HashMap<ImageId, GlaImageKey> {
        &self.bindings
    }

    pub fn binding(&self, id: ImageId) -> Option<GlaImageKey> {
        self.bindings.get(&id).copied()
    }

    pub fn version(&self) -> DocumentVersionId {
        self.version
    }

    pub fn root_binding(&self) -> Option<GlaImageKey> {
        self.bindings.get(&self.root).copied()
    }

    pub fn bump_version(&mut self) -> DocumentVersionId {
        self.version = self.version.next();
        self.version
    }
}

fn validate_document(root: ImageId, roles: &HashMap<ImageId, ImageRole>) -> Result<(), DocError> {
    if roles.is_empty() {
        return Err(DocError::EmptyRegistry);
    }
    if !roles.contains_key(&root) {
        return Err(DocError::MissingRoot { root });
    }

    let reachable = collect_reachable(root, roles)?;
    for id in roles.keys().copied() {
        if !reachable.contains(&id) {
            return Err(DocError::UnreachableImage { id });
        }
    }

    validate_no_cycles_or_self_reads(roles)?;
    Ok(())
}

fn collect_reachable(
    root: ImageId,
    roles: &HashMap<ImageId, ImageRole>,
) -> Result<HashSet<ImageId>, DocError> {
    let mut scanned = HashSet::new();
    let mut frontier = vec![root];
    while let Some(id) = frontier.pop() {
        if !scanned.insert(id) {
            continue;
        }
        if let Some(ImageRole::Derived(command)) = roles.get(&id) {
            for read in &command.reads {
                if !roles.contains_key(&read.image) {
                    return Err(DocError::MissingImage { id: read.image });
                }
                frontier.push(read.image);
            }
        }
    }
    Ok(scanned)
}

fn validate_no_cycles_or_self_reads(roles: &HashMap<ImageId, ImageRole>) -> Result<(), DocError> {
    for (id, role) in roles {
        if let ImageRole::Derived(command) = role {
            for read in &command.reads {
                if read.image == *id {
                    return Err(DocError::RegistryCommandReadsDestination { dst: *id });
                }
            }
        }
    }

    let mut out_edges = HashMap::<ImageId, Vec<ImageId>>::new();
    let mut in_degree = HashMap::<ImageId, usize>::new();
    for id in roles.keys().copied() {
        in_degree.entry(id).or_insert(0);
        out_edges.entry(id).or_default();
    }
    for (id, role) in roles {
        if let ImageRole::Derived(command) = role {
            for read in &command.reads {
                out_edges.entry(*id).or_default().push(read.image);
                *in_degree.entry(read.image).or_insert(0) += 1;
            }
        }
    }

    let mut queue: Vec<ImageId> = in_degree
        .iter()
        .filter(|(_, deg)| **deg == 0)
        .map(|(id, _)| *id)
        .collect();
    let mut visited = 0usize;
    while let Some(id) = queue.pop() {
        visited += 1;
        if let Some(outs) = out_edges.get(&id) {
            for out in outs {
                let deg = in_degree.get_mut(out).unwrap();
                *deg -= 1;
                if *deg == 0 {
                    queue.push(*out);
                }
            }
        }
    }

    if visited < roles.len() {
        let cycle_image = roles
            .keys()
            .find(|id| in_degree.get(id).copied().unwrap_or(0) > 0)
            .copied()
            .unwrap_or(ImageId::new(0));
        return Err(DocError::RegistryCycle { id: cycle_image });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(value: u32) -> GlaImageKey {
        GlaImageKey::new(value, 0)
    }

    fn primitive_role() -> ImageRole {
        ImageRole::Primitive
    }

    fn simple_doc(root: ImageId) -> Document {
        Document::new(
            root,
            HashMap::from([(root, primitive_role())]),
            HashMap::from([(root, key(10))]),
        )
        .unwrap()
    }

    #[test]
    fn document_rejects_unreachable_images() {
        let root = ImageId::new(1);
        let extra = ImageId::new(2);
        let roles = HashMap::from([(root, primitive_role()), (extra, primitive_role())]);

        let err = Document::new(root, roles, HashMap::new()).unwrap_err();
        assert!(matches!(err, DocError::UnreachableImage { id } if id == extra));
    }

    #[test]
    fn document_rejects_cycles() {
        let a = ImageId::new(1);
        let b = ImageId::new(2);
        let roles = HashMap::from([
            (
                a,
                ImageRole::Derived(GraphCommand::new(vec![GraphRead::current(b)])),
            ),
            (
                b,
                ImageRole::Derived(GraphCommand::new(vec![GraphRead::current(a)])),
            ),
        ]);

        let err = Document::new(a, roles, HashMap::new()).unwrap_err();
        assert!(matches!(err, DocError::RegistryCycle { .. }));
    }

    #[test]
    fn document_rejects_missing_read_image() {
        let root = ImageId::new(1);
        let missing = ImageId::new(2);
        let roles = HashMap::from([(
            root,
            ImageRole::Derived(GraphCommand::new(vec![GraphRead::current(missing)])),
        )]);

        let err = Document::new(root, roles, HashMap::new()).unwrap_err();
        assert!(matches!(err, DocError::MissingImage { id } if id == missing));
    }

    #[test]
    fn document_rejects_self_read() {
        let root = ImageId::new(1);
        let roles = HashMap::from([(
            root,
            ImageRole::Derived(GraphCommand::new(vec![GraphRead::current(root)])),
        )]);

        let err = Document::new(root, roles, HashMap::new()).unwrap_err();
        assert!(matches!(
            err,
            DocError::RegistryCommandReadsDestination { .. }
        ));
    }

    #[test]
    fn document_checks_binding_coverage() {
        let root = ImageId::new(1);
        let extra = ImageId::new(2);

        let err = Document::new(
            root,
            HashMap::from([(root, primitive_role())]),
            HashMap::new(),
        )
        .unwrap_err();
        assert!(matches!(err, DocError::BindingMissing { id } if id == root));

        let err = Document::new(
            root,
            HashMap::from([(root, primitive_role())]),
            HashMap::from([(root, key(1)), (extra, key(2))]),
        )
        .unwrap_err();
        assert!(matches!(err, DocError::BindingExtra { id } if id == extra));
    }

    #[test]
    fn bump_version_advances_document_version() {
        let root = ImageId::new(1);
        let mut doc = simple_doc(root);

        assert_eq!(doc.version(), DocumentVersionId::new(0));
        assert_eq!(doc.bump_version(), DocumentVersionId::new(1));
        assert_eq!(doc.version(), DocumentVersionId::new(1));
    }
}
