use crate::{
    CURRENT_SCHEMA_VERSION, FileFingerprint, LoadedDocument, RequestDocument, WorkspaceError,
    WorkspaceRepository, append_unknown_fields, atomic_write_checked, parse_flat_document,
    parse_optional_bool, parse_string, parse_u32, quote, read_limited, require_supported_version,
    required, validate_slug,
};
use apex_domain::StableId;
use apex_secrets::SecretLeakDetector;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const COLLECTION_FILE: &str = "collection.toml";
const FOLDER_FILE: &str = "folder.toml";
const ORDER_FILE: &str = ".apex-order.toml";
const MAX_MUTATION_FILES: usize = 100_000;
const MAX_MUTATION_BYTES: u64 = 256 * 1024 * 1024;
static NEXT_GENERATED_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectoryFingerprint(u64);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollectionDocument {
    pub schema_version: u32,
    pub id: StableId,
    pub name: String,
    pub archived: bool,
    pub unknown_fields: BTreeMap<String, String>,
}

impl CollectionDocument {
    pub fn new(id: StableId, name: impl Into<String>) -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            id,
            name: name.into(),
            archived: false,
            unknown_fields: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FolderDocument {
    pub schema_version: u32,
    pub id: StableId,
    pub name: String,
    pub archived: bool,
    pub unknown_fields: BTreeMap<String, String>,
}

impl FolderDocument {
    pub fn new(id: StableId, name: impl Into<String>) -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            id,
            name: name.into(),
            archived: false,
            unknown_fields: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderDocument {
    pub schema_version: u32,
    pub items: Vec<String>,
    pub unknown_fields: BTreeMap<String, String>,
}

impl OrderDocument {
    pub fn new(items: Vec<String>) -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            items,
            unknown_fields: BTreeMap::new(),
        }
    }
}

impl WorkspaceRepository {
    pub fn collection_path(&self, slug: &str) -> Result<PathBuf, WorkspaceError> {
        validate_slug(slug)?;
        Ok(self.root().join("collections").join(slug))
    }

    pub fn folder_path(
        &self,
        collection_slug: &str,
        folders: &[String],
    ) -> Result<PathBuf, WorkspaceError> {
        let mut path = self.collection_path(collection_slug)?;
        for folder in folders {
            validate_slug(folder)?;
            path.push(folder);
        }
        Ok(path)
    }

    pub fn create_collection(
        &self,
        slug: &str,
        document: &CollectionDocument,
    ) -> Result<DirectoryFingerprint, WorkspaceError> {
        validate_document_name(&document.name)?;
        require_supported_version(document.schema_version)?;
        let target = self.collection_path(slug)?;
        create_metadata_directory(&target, COLLECTION_FILE, &format_collection(document))?;
        directory_fingerprint(&target)
    }

    pub fn load_collection(
        &self,
        slug: &str,
    ) -> Result<LoadedDocument<CollectionDocument>, WorkspaceError> {
        let path = self.collection_path(slug)?.join(COLLECTION_FILE);
        load_metadata(&path, parse_collection)
    }

    pub fn create_folder(
        &self,
        collection_slug: &str,
        folders: &[String],
        document: &FolderDocument,
    ) -> Result<DirectoryFingerprint, WorkspaceError> {
        if folders.is_empty() {
            return Err(WorkspaceError::InvalidPath(
                "folder path must contain at least one segment".to_owned(),
            ));
        }
        validate_document_name(&document.name)?;
        require_supported_version(document.schema_version)?;
        let target = self.folder_path(collection_slug, folders)?;
        let parent = target
            .parent()
            .ok_or_else(|| WorkspaceError::InvalidPath(target.display().to_string()))?;
        if !parent.is_dir() {
            return Err(WorkspaceError::InvalidPath(format!(
                "folder parent does not exist: {}",
                parent.display()
            )));
        }
        create_metadata_directory(&target, FOLDER_FILE, &format_folder(document))?;
        directory_fingerprint(&target)
    }

    pub fn load_folder(
        &self,
        collection_slug: &str,
        folders: &[String],
    ) -> Result<LoadedDocument<FolderDocument>, WorkspaceError> {
        let path = self
            .folder_path(collection_slug, folders)?
            .join(FOLDER_FILE);
        load_metadata(&path, parse_folder)
    }

    pub fn collection_fingerprint(
        &self,
        slug: &str,
    ) -> Result<DirectoryFingerprint, WorkspaceError> {
        directory_fingerprint(&self.collection_path(slug)?)
    }

    pub fn folder_fingerprint(
        &self,
        collection_slug: &str,
        folders: &[String],
    ) -> Result<DirectoryFingerprint, WorkspaceError> {
        directory_fingerprint(&self.folder_path(collection_slug, folders)?)
    }

    pub fn rename_collection(
        &self,
        source_slug: &str,
        target_slug: &str,
        expected: DirectoryFingerprint,
    ) -> Result<DirectoryFingerprint, WorkspaceError> {
        let source = self.collection_path(source_slug)?;
        let target = self.collection_path(target_slug)?;
        rename_directory_checked(&source, &target, expected)?;
        directory_fingerprint(&target)
    }

    pub fn duplicate_collection(
        &self,
        source_slug: &str,
        target_slug: &str,
        expected: DirectoryFingerprint,
    ) -> Result<DirectoryFingerprint, WorkspaceError> {
        let source = self.collection_path(source_slug)?;
        let target = self.collection_path(target_slug)?;
        copy_directory_checked(&source, &target, expected, |staged| {
            self.rekey_duplicate_tree(staged, Some(target_slug))
        })?;
        directory_fingerprint(&target)
    }

    pub fn delete_collection(
        &self,
        slug: &str,
        expected: DirectoryFingerprint,
    ) -> Result<(), WorkspaceError> {
        delete_directory_checked(&self.collection_path(slug)?, expected)
    }

    pub fn rename_folder(
        &self,
        collection_slug: &str,
        folders: &[String],
        target_slug: &str,
        expected: DirectoryFingerprint,
    ) -> Result<DirectoryFingerprint, WorkspaceError> {
        validate_slug(target_slug)?;
        let source = self.folder_path(collection_slug, folders)?;
        let parent = source
            .parent()
            .ok_or_else(|| WorkspaceError::InvalidPath(source.display().to_string()))?;
        let target = parent.join(target_slug);
        rename_directory_checked(&source, &target, expected)?;
        directory_fingerprint(&target)
    }

    pub fn move_folder(
        &self,
        collection_slug: &str,
        source_folders: &[String],
        target_parent_folders: &[String],
        target_slug: &str,
        expected: DirectoryFingerprint,
    ) -> Result<DirectoryFingerprint, WorkspaceError> {
        validate_slug(target_slug)?;
        let source = self.folder_path(collection_slug, source_folders)?;
        let target_parent = self.folder_path(collection_slug, target_parent_folders)?;
        if !target_parent.is_dir() {
            return Err(WorkspaceError::InvalidPath(format!(
                "target folder does not exist: {}",
                target_parent.display()
            )));
        }
        if target_parent.starts_with(&source) {
            return Err(WorkspaceError::InvalidPath(
                "cannot move a folder into itself".to_owned(),
            ));
        }
        let target = target_parent.join(target_slug);
        rename_directory_checked(&source, &target, expected)?;
        directory_fingerprint(&target)
    }

    pub fn duplicate_folder(
        &self,
        collection_slug: &str,
        source_folders: &[String],
        target_parent_folders: &[String],
        target_slug: &str,
        expected: DirectoryFingerprint,
    ) -> Result<DirectoryFingerprint, WorkspaceError> {
        validate_slug(target_slug)?;
        let source = self.folder_path(collection_slug, source_folders)?;
        let target_parent = self.folder_path(collection_slug, target_parent_folders)?;
        if !target_parent.is_dir() {
            return Err(WorkspaceError::InvalidPath(format!(
                "target folder does not exist: {}",
                target_parent.display()
            )));
        }
        if target_parent.starts_with(&source) {
            return Err(WorkspaceError::InvalidPath(
                "cannot duplicate a folder into itself".to_owned(),
            ));
        }
        let target = target_parent.join(target_slug);
        copy_directory_checked(&source, &target, expected, |staged| {
            self.rekey_duplicate_tree(staged, None)
        })?;
        directory_fingerprint(&target)
    }

    pub fn delete_folder(
        &self,
        collection_slug: &str,
        folders: &[String],
        expected: DirectoryFingerprint,
    ) -> Result<(), WorkspaceError> {
        delete_directory_checked(&self.folder_path(collection_slug, folders)?, expected)
    }

    pub fn set_collection_archived(
        &self,
        slug: &str,
        archived: bool,
        expected: FileFingerprint,
    ) -> Result<FileFingerprint, WorkspaceError> {
        let loaded = self.load_collection(slug)?;
        if loaded.fingerprint != expected {
            return Err(WorkspaceError::ExternalChange(loaded.path));
        }
        let mut document = loaded.value;
        document.archived = archived;
        atomic_write_checked(
            &loaded.path,
            format_collection(&document).as_bytes(),
            Some(expected),
        )
    }

    pub fn set_folder_archived(
        &self,
        collection_slug: &str,
        folders: &[String],
        archived: bool,
        expected: FileFingerprint,
    ) -> Result<FileFingerprint, WorkspaceError> {
        let loaded = self.load_folder(collection_slug, folders)?;
        if loaded.fingerprint != expected {
            return Err(WorkspaceError::ExternalChange(loaded.path));
        }
        let mut document = loaded.value;
        document.archived = archived;
        atomic_write_checked(
            &loaded.path,
            format_folder(&document).as_bytes(),
            Some(expected),
        )
    }

    pub fn load_order(
        &self,
        collection_slug: Option<&str>,
        folders: &[String],
    ) -> Result<Option<LoadedDocument<OrderDocument>>, WorkspaceError> {
        let directory = match collection_slug {
            Some(collection) => self.folder_path(collection, folders)?,
            None if folders.is_empty() => self.root().join("collections"),
            None => {
                return Err(WorkspaceError::InvalidPath(
                    "workspace collection order cannot include folder segments".to_owned(),
                ));
            }
        };
        let path = directory.join(ORDER_FILE);
        if !path.exists() {
            return Ok(None);
        }
        load_metadata(&path, parse_order).map(Some)
    }

    pub fn save_order(
        &self,
        collection_slug: Option<&str>,
        folders: &[String],
        document: &OrderDocument,
        expected: Option<FileFingerprint>,
    ) -> Result<FileFingerprint, WorkspaceError> {
        require_supported_version(document.schema_version)?;
        validate_order_items(&document.items)?;
        let directory = match collection_slug {
            Some(collection) => self.folder_path(collection, folders)?,
            None if folders.is_empty() => self.root().join("collections"),
            None => {
                return Err(WorkspaceError::InvalidPath(
                    "workspace collection order cannot include folder segments".to_owned(),
                ));
            }
        };
        if !directory.is_dir() {
            return Err(WorkspaceError::InvalidPath(format!(
                "order directory does not exist: {}",
                directory.display()
            )));
        }
        for item in &document.items {
            if !directory.join(item).exists() {
                return Err(WorkspaceError::InvalidPath(format!(
                    "ordered item does not exist: {item}"
                )));
            }
        }
        atomic_write_checked(
            &directory.join(ORDER_FILE),
            format_order(document).as_bytes(),
            expected,
        )
    }

    pub(crate) fn list_request_files_ordered(&self) -> Result<Vec<PathBuf>, WorkspaceError> {
        let root = self.root().join("collections");
        if !root.exists() {
            return Ok(Vec::new());
        }
        let mut output = Vec::new();
        for entry in ordered_entries(&root)? {
            if entry.file_type()?.is_symlink() {
                return Err(WorkspaceError::SymbolicLink(entry.path()));
            }
            if !entry.file_type()?.is_dir() || is_internal_name(&entry.file_name()) {
                continue;
            }
            let path = entry.path();
            if metadata_archived(&path.join(COLLECTION_FILE), parse_collection)? {
                continue;
            }
            collect_request_files(&path, true, &mut output)?;
        }
        Ok(output)
    }

    fn rekey_duplicate_tree(
        &self,
        root: &Path,
        collection_slug: Option<&str>,
    ) -> Result<(), WorkspaceError> {
        let collection_path = root.join(COLLECTION_FILE);
        if collection_path.exists() {
            let loaded = load_metadata(&collection_path, parse_collection)?;
            let mut document = loaded.value;
            document.id = generated_id("collection")?;
            if let Some(slug) = collection_slug {
                document.name = humanize_slug(slug);
            }
            atomic_write_checked(
                &collection_path,
                format_collection(&document).as_bytes(),
                Some(loaded.fingerprint),
            )?;
        } else if let Some(slug) = collection_slug {
            let document =
                CollectionDocument::new(generated_id("collection")?, humanize_slug(slug));
            atomic_write_checked(
                &collection_path,
                format_collection(&document).as_bytes(),
                None,
            )?;
        }

        for path in walk_tree_files(root)? {
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if file_name == FOLDER_FILE {
                let loaded = load_metadata(&path, parse_folder)?;
                let mut document = loaded.value;
                document.id = generated_id("folder")?;
                atomic_write_checked(
                    &path,
                    format_folder(&document).as_bytes(),
                    Some(loaded.fingerprint),
                )?;
            } else if file_name.ends_with(".request.toml") {
                let loaded = self.load_request(&path)?;
                let mut document: RequestDocument = loaded.value;
                document.request.id = generated_id("request")?;
                self.save_request(
                    &path,
                    &document,
                    Some(loaded.fingerprint),
                    &SecretLeakDetector::default(),
                )?;
            }
        }
        Ok(())
    }
}

pub fn format_collection(document: &CollectionDocument) -> String {
    format_named_metadata(
        document.schema_version,
        "collection_id",
        &document.id,
        &document.name,
        document.archived,
        &document.unknown_fields,
    )
}

pub fn parse_collection(input: &str) -> Result<CollectionDocument, WorkspaceError> {
    let mut values = parse_flat_document(input)?;
    let schema_version = parse_u32(required(&values, "schema_version")?, "schema_version")?;
    require_supported_version(schema_version)?;
    let id = StableId::parse(parse_string(required(&values, "collection_id")?)?)
        .map_err(|error| WorkspaceError::InvalidFormat(error.to_string()))?;
    let name = parse_string(required(&values, "name")?)?;
    validate_document_name(&name)?;
    let archived = parse_optional_bool(&values, "archived", false)?;
    for key in ["schema_version", "collection_id", "name", "archived"] {
        values.remove(key);
    }
    Ok(CollectionDocument {
        schema_version,
        id,
        name,
        archived,
        unknown_fields: values,
    })
}

pub fn format_folder(document: &FolderDocument) -> String {
    format_named_metadata(
        document.schema_version,
        "folder_id",
        &document.id,
        &document.name,
        document.archived,
        &document.unknown_fields,
    )
}

pub fn parse_folder(input: &str) -> Result<FolderDocument, WorkspaceError> {
    let mut values = parse_flat_document(input)?;
    let schema_version = parse_u32(required(&values, "schema_version")?, "schema_version")?;
    require_supported_version(schema_version)?;
    let id = StableId::parse(parse_string(required(&values, "folder_id")?)?)
        .map_err(|error| WorkspaceError::InvalidFormat(error.to_string()))?;
    let name = parse_string(required(&values, "name")?)?;
    validate_document_name(&name)?;
    let archived = parse_optional_bool(&values, "archived", false)?;
    for key in ["schema_version", "folder_id", "name", "archived"] {
        values.remove(key);
    }
    Ok(FolderDocument {
        schema_version,
        id,
        name,
        archived,
        unknown_fields: values,
    })
}

pub fn format_order(document: &OrderDocument) -> String {
    let mut output = format!(
        "schema_version = {}\nitems = {}\n",
        document.schema_version,
        serde_json::to_string(&document.items).expect("string arrays serialize")
    );
    append_unknown_fields(&mut output, &document.unknown_fields);
    output
}

pub fn parse_order(input: &str) -> Result<OrderDocument, WorkspaceError> {
    let mut values = parse_flat_document(input)?;
    let schema_version = parse_u32(required(&values, "schema_version")?, "schema_version")?;
    require_supported_version(schema_version)?;
    let items = serde_json::from_str::<Vec<String>>(required(&values, "items")?)
        .map_err(|error| WorkspaceError::InvalidFormat(format!("invalid order items: {error}")))?;
    validate_order_items(&items)?;
    values.remove("schema_version");
    values.remove("items");
    Ok(OrderDocument {
        schema_version,
        items,
        unknown_fields: values,
    })
}

fn format_named_metadata(
    schema_version: u32,
    id_key: &str,
    id: &StableId,
    name: &str,
    archived: bool,
    unknown_fields: &BTreeMap<String, String>,
) -> String {
    let mut output = format!(
        "schema_version = {schema_version}\n{id_key} = {}\nname = {}\narchived = {archived}\n",
        quote(id.as_str()),
        quote(name)
    );
    append_unknown_fields(&mut output, unknown_fields);
    output
}

fn validate_document_name(name: &str) -> Result<(), WorkspaceError> {
    if name.trim().is_empty() || name.chars().any(char::is_control) {
        Err(WorkspaceError::InvalidFormat(
            "collection and folder names must be non-empty and contain no control characters"
                .to_owned(),
        ))
    } else {
        Ok(())
    }
}

fn validate_order_items(items: &[String]) -> Result<(), WorkspaceError> {
    let mut seen = BTreeSet::new();
    for item in items {
        let path = Path::new(item);
        if item.is_empty()
            || path.components().count() != 1
            || !matches!(path.components().next(), Some(Component::Normal(_)))
            || is_internal_name(path.as_os_str())
        {
            return Err(WorkspaceError::InvalidPath(format!(
                "invalid order item: {item}"
            )));
        }
        if !seen.insert(item) {
            return Err(WorkspaceError::InvalidFormat(format!(
                "duplicate order item: {item}"
            )));
        }
    }
    Ok(())
}

fn create_metadata_directory(
    target: &Path,
    metadata_name: &str,
    metadata: &str,
) -> Result<(), WorkspaceError> {
    if target.exists() {
        return Err(WorkspaceError::AlreadyExists(target.to_owned()));
    }
    let parent = target
        .parent()
        .ok_or_else(|| WorkspaceError::InvalidPath(target.display().to_string()))?;
    fs::create_dir_all(parent)?;
    let temporary = temporary_sibling(target, "create")?;
    let result = (|| {
        fs::create_dir(&temporary)?;
        atomic_write_checked(&temporary.join(metadata_name), metadata.as_bytes(), None)?;
        sync_directory(&temporary)?;
        fs::rename(&temporary, target)?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&temporary);
    }
    result
}

fn rename_directory_checked(
    source: &Path,
    target: &Path,
    expected: DirectoryFingerprint,
) -> Result<(), WorkspaceError> {
    ensure_directory_snapshot(source, expected)?;
    if target.exists() {
        return Err(WorkspaceError::AlreadyExists(target.to_owned()));
    }
    let target_parent = target
        .parent()
        .ok_or_else(|| WorkspaceError::InvalidPath(target.display().to_string()))?;
    if !target_parent.is_dir() {
        return Err(WorkspaceError::InvalidPath(format!(
            "target parent does not exist: {}",
            target_parent.display()
        )));
    }
    fs::rename(source, target)?;
    sync_directory(target_parent)?;
    if source.parent() != Some(target_parent)
        && let Some(source_parent) = source.parent()
    {
        sync_directory(source_parent)?;
    }
    Ok(())
}

fn copy_directory_checked(
    source: &Path,
    target: &Path,
    expected: DirectoryFingerprint,
    prepare: impl FnOnce(&Path) -> Result<(), WorkspaceError>,
) -> Result<(), WorkspaceError> {
    ensure_directory_snapshot(source, expected)?;
    if target.exists() {
        return Err(WorkspaceError::AlreadyExists(target.to_owned()));
    }
    let parent = target
        .parent()
        .ok_or_else(|| WorkspaceError::InvalidPath(target.display().to_string()))?;
    let temporary = temporary_sibling(target, "copy")?;
    let result = (|| {
        copy_directory_tree(source, &temporary)?;
        if directory_fingerprint(source)? != expected {
            return Err(WorkspaceError::ExternalChange(source.to_owned()));
        }
        prepare(&temporary)?;
        fs::rename(&temporary, target)?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&temporary);
    }
    result
}

fn delete_directory_checked(
    source: &Path,
    expected: DirectoryFingerprint,
) -> Result<(), WorkspaceError> {
    ensure_directory_snapshot(source, expected)?;
    let parent = source
        .parent()
        .ok_or_else(|| WorkspaceError::InvalidPath(source.display().to_string()))?;
    let tombstone = temporary_sibling(source, "delete")?;
    fs::rename(source, &tombstone)?;
    sync_directory(parent)?;
    if let Err(error) = fs::remove_dir_all(&tombstone) {
        let restore = fs::rename(&tombstone, source);
        let _ = sync_directory(parent);
        return match restore {
            Ok(()) => Err(WorkspaceError::Io(error)),
            Err(restore_error) => Err(WorkspaceError::InvalidFormat(format!(
                "deletion failed ({error}) and rollback failed ({restore_error}); recover {}",
                tombstone.display()
            ))),
        };
    }
    sync_directory(parent)
}

fn ensure_directory_snapshot(
    path: &Path,
    expected: DirectoryFingerprint,
) -> Result<(), WorkspaceError> {
    if directory_fingerprint(path)? == expected {
        Ok(())
    } else {
        Err(WorkspaceError::ExternalChange(path.to_owned()))
    }
}

fn directory_fingerprint(path: &Path) -> Result<DirectoryFingerprint, WorkspaceError> {
    if !path.is_dir() {
        return Err(WorkspaceError::InvalidPath(format!(
            "workspace directory does not exist: {}",
            path.display()
        )));
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut files = 0_usize;
    let mut total_bytes = 0_u64;
    hash_directory_tree(path, path, &mut hasher, &mut files, &mut total_bytes)?;
    Ok(DirectoryFingerprint(hasher.finish()))
}

fn hash_directory_tree(
    root: &Path,
    directory: &Path,
    hasher: &mut impl Hasher,
    files: &mut usize,
    total_bytes: &mut u64,
) -> Result<(), WorkspaceError> {
    for entry in ordered_raw_entries(directory)? {
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|_| WorkspaceError::PathTraversal(path.display().to_string()))?;
        relative.hash(hasher);
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(WorkspaceError::SymbolicLink(path));
        }
        if file_type.is_dir() {
            0_u8.hash(hasher);
            hash_directory_tree(root, &path, hasher, files, total_bytes)?;
        } else if file_type.is_file() {
            1_u8.hash(hasher);
            *files += 1;
            if *files > MAX_MUTATION_FILES {
                return Err(WorkspaceError::InvalidFormat(format!(
                    "workspace mutation exceeds {MAX_MUTATION_FILES} files"
                )));
            }
            let metadata = entry.metadata()?;
            *total_bytes = total_bytes.saturating_add(metadata.len());
            if *total_bytes > MAX_MUTATION_BYTES {
                return Err(WorkspaceError::FileTooLarge {
                    path: root.to_owned(),
                    maximum_bytes: MAX_MUTATION_BYTES,
                    observed_bytes: *total_bytes,
                });
            }
            let mut input = File::open(&path)?;
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let read = input.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                buffer[..read].hash(hasher);
            }
        }
    }
    Ok(())
}

fn walk_tree_files(root: &Path) -> Result<Vec<PathBuf>, WorkspaceError> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let path = entry.path();
            if file_type.is_symlink() {
                return Err(WorkspaceError::SymbolicLink(path));
            }
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file() {
                files.push(path);
                if files.len() > MAX_MUTATION_FILES {
                    return Err(WorkspaceError::InvalidFormat(format!(
                        "workspace mutation exceeds {MAX_MUTATION_FILES} files"
                    )));
                }
            }
        }
    }
    files.sort();
    Ok(files)
}

fn copy_directory_tree(source: &Path, target: &Path) -> Result<(), WorkspaceError> {
    fs::create_dir(target)?;
    let source_permissions = fs::metadata(source)?.permissions();
    for entry in ordered_raw_entries(source)? {
        let file_type = entry.file_type()?;
        let source_path = entry.path();
        let destination = target.join(entry.file_name());
        if file_type.is_symlink() {
            return Err(WorkspaceError::SymbolicLink(source_path));
        }
        if file_type.is_dir() {
            copy_directory_tree(&source_path, &destination)?;
        } else if file_type.is_file() {
            let source_metadata = entry.metadata()?;
            let mut source_file = File::open(&source_path)?;
            let mut target_file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)?;
            std::io::copy(&mut source_file, &mut target_file)?;
            target_file.sync_all()?;
            drop(target_file);
            fs::set_permissions(&destination, source_metadata.permissions())?;
        }
    }
    fs::set_permissions(target, source_permissions)?;
    sync_directory(target)
}

fn collect_request_files(
    directory: &Path,
    collection_root: bool,
    output: &mut Vec<PathBuf>,
) -> Result<(), WorkspaceError> {
    if !collection_root && metadata_archived(&directory.join(FOLDER_FILE), parse_folder)? {
        return Ok(());
    }
    for entry in ordered_entries(directory)? {
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(WorkspaceError::SymbolicLink(entry.path()));
        }
        if is_internal_name(&entry.file_name()) {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            collect_request_files(&path, false, output)?;
        } else if file_type.is_file()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".request.toml"))
        {
            output.push(path);
        }
    }
    Ok(())
}

fn ordered_entries(directory: &Path) -> Result<Vec<fs::DirEntry>, WorkspaceError> {
    let mut entries = ordered_raw_entries(directory)?;
    let order = read_order_items(&directory.join(ORDER_FILE))?;
    let positions = order
        .iter()
        .enumerate()
        .map(|(index, value)| (value.as_str(), index))
        .collect::<BTreeMap<_, _>>();
    entries.sort_by(|left, right| {
        let left_name = left.file_name().to_string_lossy().into_owned();
        let right_name = right.file_name().to_string_lossy().into_owned();
        let left_position = positions.get(left_name.as_str()).copied();
        let right_position = positions.get(right_name.as_str()).copied();
        left_position
            .is_none()
            .cmp(&right_position.is_none())
            .then_with(|| left_position.cmp(&right_position))
            .then_with(|| left_name.cmp(&right_name))
    });
    Ok(entries)
}

fn ordered_raw_entries(directory: &Path) -> Result<Vec<fs::DirEntry>, WorkspaceError> {
    let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries)
}

fn read_order_items(path: &Path) -> Result<Vec<String>, WorkspaceError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = read_limited(path, 4 * 1024 * 1024)?;
    let content =
        std::str::from_utf8(&bytes).map_err(|_| WorkspaceError::InvalidUtf8(path.to_owned()))?;
    Ok(parse_order(content)?.items)
}

fn metadata_archived<T>(
    path: &Path,
    parse: fn(&str) -> Result<T, WorkspaceError>,
) -> Result<bool, WorkspaceError>
where
    T: ArchivedMetadata,
{
    if !path.exists() {
        return Ok(false);
    }
    let loaded = load_metadata(path, parse)?;
    Ok(loaded.value.archived())
}

trait ArchivedMetadata {
    fn archived(&self) -> bool;
}

impl ArchivedMetadata for CollectionDocument {
    fn archived(&self) -> bool {
        self.archived
    }
}

impl ArchivedMetadata for FolderDocument {
    fn archived(&self) -> bool {
        self.archived
    }
}

fn load_metadata<T>(
    path: &Path,
    parse: fn(&str) -> Result<T, WorkspaceError>,
) -> Result<LoadedDocument<T>, WorkspaceError> {
    let bytes = read_limited(path, 4 * 1024 * 1024)?;
    let content =
        std::str::from_utf8(&bytes).map_err(|_| WorkspaceError::InvalidUtf8(path.to_owned()))?;
    crate::detect_conflict_error(path, content)?;
    let value = parse(content)?;
    Ok(LoadedDocument {
        value,
        path: path.to_owned(),
        fingerprint: FileFingerprint::from_bytes(&bytes),
    })
}

fn temporary_sibling(path: &Path, operation: &str) -> Result<PathBuf, WorkspaceError> {
    let parent = path
        .parent()
        .ok_or_else(|| WorkspaceError::InvalidPath(path.display().to_string()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| WorkspaceError::InvalidPath(path.display().to_string()))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(parent.join(format!(".{file_name}.{operation}.{nonce}.tmp")))
}

fn sync_directory(path: &Path) -> Result<(), WorkspaceError> {
    if let Ok(directory) = File::open(path) {
        directory.sync_all()?;
    }
    Ok(())
}

fn generated_id(prefix: &str) -> Result<StableId, WorkspaceError> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = NEXT_GENERATED_ID.fetch_add(1, Ordering::Relaxed);
    StableId::parse(format!("{prefix}-{timestamp:x}-{sequence:x}"))
        .map_err(|error| WorkspaceError::InvalidFormat(error.to_string()))
}

fn humanize_slug(slug: &str) -> String {
    slug.split(['-', '_'])
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            let mut characters = segment.chars();
            characters.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + characters.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_internal_name(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return true;
    };
    name == COLLECTION_FILE || name == FOLDER_FILE || name == ORDER_FILE || name.starts_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;
    use apex_domain::{Authentication, HttpMethod, HttpRequest, RequestBody, RequestSettings};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn collection_round_trip_and_rename_preserve_identity() {
        let (repository, root) = fixture_repository("rename");
        let document = CollectionDocument::new(stable("users"), "Users API");
        let fingerprint = repository
            .create_collection("users", &document)
            .expect("create collection");
        repository
            .rename_collection("users", "accounts", fingerprint)
            .expect("rename collection");
        let loaded = repository
            .load_collection("accounts")
            .expect("load renamed collection");
        assert_eq!(loaded.value.id, stable("users"));
        assert!(!repository.collection_path("users").unwrap().exists());
        cleanup(root);
    }

    #[test]
    fn duplicate_collection_rekeys_collection_folder_and_request() {
        let (repository, root) = fixture_repository("duplicate");
        repository
            .create_collection(
                "users",
                &CollectionDocument::new(stable("collection-source"), "Users"),
            )
            .expect("collection");
        repository
            .create_folder(
                "users",
                &["admin".to_owned()],
                &FolderDocument::new(stable("folder-source"), "Admin"),
            )
            .expect("folder");
        let request_path = repository
            .folder_path("users", &["admin".to_owned()])
            .unwrap()
            .join("get.request.toml");
        repository
            .save_request(
                &request_path,
                &RequestDocument::new(request("request-source", "Get User")),
                None,
                &SecretLeakDetector::default(),
            )
            .expect("request");
        let source_fingerprint = repository.collection_fingerprint("users").unwrap();
        repository
            .duplicate_collection("users", "users-copy", source_fingerprint)
            .expect("duplicate");

        let copied_collection = repository.load_collection("users-copy").unwrap();
        let copied_folder = repository
            .load_folder("users-copy", &["admin".to_owned()])
            .unwrap();
        let copied_request = repository
            .load_request(
                &repository
                    .folder_path("users-copy", &["admin".to_owned()])
                    .unwrap()
                    .join("get.request.toml"),
            )
            .unwrap();
        assert_ne!(copied_collection.value.id, stable("collection-source"));
        assert_ne!(copied_folder.value.id, stable("folder-source"));
        assert_ne!(copied_request.value.request.id, stable("request-source"));
        cleanup(root);
    }

    #[test]
    fn archived_collection_and_folder_are_excluded_from_request_index() {
        let (repository, root) = fixture_repository("archive");
        repository
            .create_collection("users", &CollectionDocument::new(stable("users"), "Users"))
            .unwrap();
        repository
            .create_folder(
                "users",
                &["admin".to_owned()],
                &FolderDocument::new(stable("admin"), "Admin"),
            )
            .unwrap();
        save_fixture_request(&repository, "users", &["admin".to_owned()], "get");
        assert_eq!(repository.list_requests().unwrap().len(), 1);

        let folder = repository
            .load_folder("users", &["admin".to_owned()])
            .unwrap();
        repository
            .set_folder_archived("users", &["admin".to_owned()], true, folder.fingerprint)
            .unwrap();
        assert!(repository.list_requests().unwrap().is_empty());

        let folder = repository
            .load_folder("users", &["admin".to_owned()])
            .unwrap();
        repository
            .set_folder_archived("users", &["admin".to_owned()], false, folder.fingerprint)
            .unwrap();
        let collection = repository.load_collection("users").unwrap();
        repository
            .set_collection_archived("users", true, collection.fingerprint)
            .unwrap();
        assert!(repository.list_requests().unwrap().is_empty());
        cleanup(root);
    }

    #[test]
    fn move_and_rename_folder_are_atomic_and_path_safe() {
        let (repository, root) = fixture_repository("move");
        repository
            .create_collection("users", &CollectionDocument::new(stable("users"), "Users"))
            .unwrap();
        repository
            .create_folder(
                "users",
                &["source".to_owned()],
                &FolderDocument::new(stable("source"), "Source"),
            )
            .unwrap();
        repository
            .create_folder(
                "users",
                &["target".to_owned()],
                &FolderDocument::new(stable("target"), "Target"),
            )
            .unwrap();
        let fingerprint = repository
            .folder_fingerprint("users", &["source".to_owned()])
            .unwrap();
        repository
            .move_folder(
                "users",
                &["source".to_owned()],
                &["target".to_owned()],
                "moved",
                fingerprint,
            )
            .unwrap();
        let moved = vec!["target".to_owned(), "moved".to_owned()];
        let fingerprint = repository.folder_fingerprint("users", &moved).unwrap();
        repository
            .rename_folder("users", &moved, "renamed", fingerprint)
            .unwrap();
        assert!(
            repository
                .folder_path("users", &["target".to_owned(), "renamed".to_owned()])
                .unwrap()
                .exists()
        );
        assert!(
            repository
                .move_folder(
                    "users",
                    &["target".to_owned()],
                    &["target".to_owned(), "renamed".to_owned()],
                    "invalid",
                    repository
                        .folder_fingerprint("users", &["target".to_owned()])
                        .unwrap(),
                )
                .is_err()
        );
        cleanup(root);
    }

    #[test]
    fn order_file_controls_request_index_without_renaming_files() {
        let (repository, root) = fixture_repository("order");
        repository
            .create_collection("users", &CollectionDocument::new(stable("users"), "Users"))
            .unwrap();
        save_fixture_request(&repository, "users", &[], "alpha");
        save_fixture_request(&repository, "users", &[], "beta");
        repository
            .save_order(
                Some("users"),
                &[],
                &OrderDocument::new(vec![
                    "beta.request.toml".to_owned(),
                    "alpha.request.toml".to_owned(),
                ]),
                None,
            )
            .unwrap();
        let requests = repository.list_requests().unwrap();
        assert_eq!(requests[0].slug, "beta");
        assert_eq!(requests[1].slug, "alpha");
        cleanup(root);
    }

    #[test]
    fn directory_fingerprint_detects_empty_folder_changes() {
        let (repository, root) = fixture_repository("empty-folder-fingerprint");
        repository
            .create_collection("users", &CollectionDocument::new(stable("users"), "Users"))
            .unwrap();
        let fingerprint = repository.collection_fingerprint("users").unwrap();
        fs::create_dir(
            repository
                .collection_path("users")
                .unwrap()
                .join("external-empty"),
        )
        .unwrap();
        assert!(matches!(
            repository.rename_collection("users", "accounts", fingerprint),
            Err(WorkspaceError::ExternalChange(_))
        ));
        cleanup(root);
    }

    #[test]
    fn stale_directory_fingerprint_rejects_destructive_mutation() {
        let (repository, root) = fixture_repository("stale");
        repository
            .create_collection("users", &CollectionDocument::new(stable("users"), "Users"))
            .unwrap();
        let fingerprint = repository.collection_fingerprint("users").unwrap();
        fs::write(
            repository
                .collection_path("users")
                .unwrap()
                .join("external.txt"),
            "external edit",
        )
        .unwrap();
        assert!(matches!(
            repository.rename_collection("users", "accounts", fingerprint),
            Err(WorkspaceError::ExternalChange(_))
        ));
        assert!(repository.collection_path("users").unwrap().exists());
        cleanup(root);
    }

    #[test]
    fn delete_uses_snapshot_guard_and_removes_the_tree() {
        let (repository, root) = fixture_repository("delete");
        let fingerprint = repository
            .create_collection("users", &CollectionDocument::new(stable("users"), "Users"))
            .unwrap();
        repository
            .delete_collection("users", fingerprint)
            .expect("delete collection");
        assert!(!repository.collection_path("users").unwrap().exists());
        cleanup(root);
    }

    #[test]
    fn failed_duplicate_does_not_publish_partial_destination() {
        let (repository, root) = fixture_repository("duplicate-rollback");
        repository
            .create_collection("users", &CollectionDocument::new(stable("users"), "Users"))
            .unwrap();
        fs::write(
            repository
                .collection_path("users")
                .unwrap()
                .join("broken.request.toml"),
            "not a request document",
        )
        .unwrap();
        let fingerprint = repository.collection_fingerprint("users").unwrap();
        assert!(
            repository
                .duplicate_collection("users", "users-copy", fingerprint)
                .is_err()
        );
        assert!(!repository.collection_path("users-copy").unwrap().exists());
        cleanup(root);
    }

    #[cfg(unix)]
    #[test]
    fn duplicate_rejects_symbolic_links() {
        use std::os::unix::fs::symlink;

        let (repository, root) = fixture_repository("symlink");
        repository
            .create_collection("users", &CollectionDocument::new(stable("users"), "Users"))
            .unwrap();
        symlink(
            root.join("apex.toml"),
            repository.collection_path("users").unwrap().join("escape"),
        )
        .unwrap();
        assert!(matches!(
            repository.collection_fingerprint("users"),
            Err(WorkspaceError::SymbolicLink(_))
        ));
        cleanup(root);
    }

    fn fixture_repository(name: &str) -> (WorkspaceRepository, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "apex-collection-{name}-{}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let repository = WorkspaceRepository::new(&root).unwrap();
        repository
            .initialize(&crate::WorkspaceManifest::new(
                stable("workspace"),
                "Fixture",
            ))
            .unwrap();
        (repository, root)
    }

    fn save_fixture_request(
        repository: &WorkspaceRepository,
        collection: &str,
        folders: &[String],
        slug: &str,
    ) {
        let path = repository
            .folder_path(collection, folders)
            .unwrap()
            .join(format!("{slug}.request.toml"));
        repository
            .save_request(
                &path,
                &RequestDocument::new(request(slug, slug)),
                None,
                &SecretLeakDetector::default(),
            )
            .unwrap();
    }

    fn request(id: &str, name: &str) -> HttpRequest {
        HttpRequest {
            id: stable(id),
            name: name.to_owned(),
            method: HttpMethod::Get,
            url: "https://example.test".to_owned(),
            query: Vec::new(),
            headers: Vec::new(),
            authentication: Authentication::None,
            body: RequestBody::Empty,
            settings: RequestSettings::default(),
            documentation: String::new(),
        }
    }

    fn stable(value: &str) -> StableId {
        StableId::parse(value).unwrap()
    }

    fn cleanup(root: PathBuf) {
        fs::remove_dir_all(root).unwrap();
    }
}
