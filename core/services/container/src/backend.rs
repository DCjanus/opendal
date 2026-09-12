// Licensed to the Apache Software Foundation (ASF) under one
// or more contributor license agreements.  See the NOTICE file
// distributed with this work for additional information
// regarding copyright ownership.  The ASF licenses this file
// to you under the Apache License, Version 2.0 (the
// "License"); you may not use this file except in compliance
// with the License.  You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing,
// software distributed under the License is distributed on an
// "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied.  See the License for the
// specific language governing permissions and limitations
// under the License.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use flate2::read::GzDecoder;
use opendal_core::raw::*;
use opendal_core::*;
use serde::Deserialize;
use tar::Archive;

use super::CONTAINER_SCHEME;
use super::config::ContainerConfig;
use super::config::ImagePlatform;
use super::config::OciPlatform;
use super::core::ContainerCore;
use super::core::ContainerEntry;
use super::core::ContainerLink;
use super::lister::ContainerLister;

const OCI_REF_NAME: &str = "org.opencontainers.image.ref.name";
const OCI_IMAGE_INDEX: &str = "application/vnd.oci.image.index.v1+json";
const OCI_IMAGE_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const OCI_LAYER_TAR: &str = "application/vnd.oci.image.layer.v1.tar";
const OCI_LAYER_TAR_GZIP: &str = "application/vnd.oci.image.layer.v1.tar+gzip";
const DOCKER_MANIFEST_LIST: &str = "application/vnd.docker.distribution.manifest.list.v2+json";
const DOCKER_MANIFEST: &str = "application/vnd.docker.distribution.manifest.v2+json";
const DOCKER_LAYER_TAR_GZIP: &str = "application/vnd.docker.image.rootfs.diff.tar.gzip";

/// Experimental container image backend support.
#[doc = include_str!("docs.md")]
#[derive(Debug, Default)]
pub struct ContainerBuilder {
    pub(super) config: ContainerConfig,
}

impl ContainerBuilder {
    /// Set the local OCI image layout directory.
    pub fn layout(mut self, layout: impl Into<String>) -> Self {
        self.config.layout = Some(layout.into());
        self
    }

    /// Set the image reference name in `index.json`.
    pub fn reference(mut self, reference: impl Into<String>) -> Self {
        self.config.reference = Some(reference.into());
        self
    }

    /// Set the image platform selector.
    pub fn platform(mut self, platform: ImagePlatform) -> Self {
        self.config.platform = Some(platform);
        self
    }

    /// Set root inside the merged image rootfs.
    pub fn root(mut self, root: &str) -> Self {
        self.config.root = if root.is_empty() {
            None
        } else {
            Some(root.to_string())
        };
        self
    }
}

impl Builder for ContainerBuilder {
    type Config = ContainerConfig;

    fn build(self) -> Result<impl Service> {
        let layout = self.config.layout.ok_or_else(|| {
            Error::new(ErrorKind::ConfigInvalid, "layout is not specified")
                .with_context("service", CONTAINER_SCHEME)
        })?;
        let reference = self.config.reference.ok_or_else(|| {
            Error::new(ErrorKind::ConfigInvalid, "reference is not specified")
                .with_context("service", CONTAINER_SCHEME)
        })?;
        let platform = self.config.platform.ok_or_else(|| {
            Error::new(ErrorKind::ConfigInvalid, "platform is not specified")
                .with_context("service", CONTAINER_SCHEME)
        })?;
        let root = normalize_root(self.config.root.as_deref().unwrap_or("/"));
        let layout = PathBuf::from(layout);
        let core = load_layout(&layout, &reference, &platform)?;

        let info = ServiceInfo::new(CONTAINER_SCHEME, &root, &reference);
        let capability = Capability {
            stat: true,
            read: true,
            list: true,
            list_with_recursive: true,
            shared: true,
            ..Default::default()
        };

        Ok(ContainerBackend {
            core: Arc::new(core),
            root,
            info,
            capability,
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ContainerBackend {
    core: Arc<ContainerCore>,
    root: String,
    info: ServiceInfo,
    capability: Capability,
}

impl ContainerBackend {
    fn get(&self, path: &str) -> Result<Option<ContainerEntry>> {
        let Some(resolved) = self.core.resolve_path(path)? else {
            return Ok(None);
        };
        let root = self.root.trim_start_matches('/');
        if !root.is_empty() && resolved != root.trim_end_matches('/') && !resolved.starts_with(root)
        {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                "container image link escapes the configured root",
            ));
        }
        Ok(self.core.entries.get(&resolved).cloned())
    }
}

impl Service for ContainerBackend {
    type Reader = oio::StreamReader<ContainerReader>;
    type Writer = ();
    type Lister = oio::HierarchyLister<ContainerLister>;
    type Deleter = ();
    type Copier = ();
    type Composer = ();

    fn info(&self) -> ServiceInfo {
        self.info.clone()
    }

    fn capability(&self) -> Capability {
        self.capability
    }

    async fn stat(&self, _: &OperationContext, path: &str, _: OpStat) -> Result<RpStat> {
        let p = build_abs_path(&self.root, path);
        if p == self.root[1..] || p.is_empty() {
            return Ok(RpStat::new(MetadataBuilder::dir().build()));
        }

        if let Some(entry) = self.get(&p)? {
            return Ok(RpStat::new(entry.metadata));
        }

        let dir_path = if p.ends_with('/') { p } else { format!("{p}/") };
        if self.core.has_children(&dir_path) {
            return Ok(RpStat::new(MetadataBuilder::dir().build()));
        }

        Err(Error::new(
            ErrorKind::NotFound,
            "path is not found in container image",
        ))
    }

    fn read(&self, _: &OperationContext, path: &str, _: OpRead) -> Result<Self::Reader> {
        Ok(oio::StreamReader::new(ContainerReader {
            backend: self.clone(),
            path: path.to_string(),
        }))
    }

    fn list(&self, _: &OperationContext, path: &str, args: OpList) -> Result<Self::Lister> {
        let p = build_abs_path(&self.root, path);
        let scan_path = if p.is_empty() || p.ends_with('/') {
            p
        } else {
            format!("{p}/")
        };
        let entries = self.core.scan(&scan_path);
        let lister = ContainerLister::new(self.root.clone(), entries);
        let lister = oio::HierarchyLister::new(lister, path, args.recursive());

        Ok(lister)
    }

    async fn create_dir(
        &self,
        _: &OperationContext,
        _: &str,
        _: OpCreateDir,
    ) -> Result<RpCreateDir> {
        Err(unsupported())
    }

    fn write(&self, _: &OperationContext, _: &str, _: OpWrite) -> Result<Self::Writer> {
        Err(unsupported())
    }

    fn delete(&self, _: &OperationContext) -> Result<Self::Deleter> {
        Err(unsupported())
    }

    fn copy(&self, _: &OperationContext, _: &str, _: &str, _: OpCopy) -> Result<Self::Copier> {
        Err(unsupported())
    }

    async fn rename(
        &self,
        _: &OperationContext,
        _: &str,
        _: &str,
        _: OpRename,
    ) -> Result<RpRename> {
        Err(unsupported())
    }

    async fn presign(&self, _: &OperationContext, _: &str, _: OpPresign) -> Result<RpPresign> {
        Err(unsupported())
    }
}

/// Reader returned by the container backend.
pub struct ContainerReader {
    backend: ContainerBackend,
    path: String,
}

impl oio::StreamRead for ContainerReader {
    async fn open(&self, range: BytesRange) -> Result<(RpRead, Box<dyn oio::ReadStreamDyn>)> {
        let path = build_abs_path(&self.backend.root, &self.path);
        let entry = self.backend.get(&path)?.ok_or_else(|| {
            Error::new(ErrorKind::NotFound, "path is not found in container image")
        })?;
        let content = entry.content.ok_or_else(|| {
            Error::new(
                ErrorKind::IsADirectory,
                "path is not a readable regular file",
            )
        })?;
        let total_size = content.len() as u64;
        let content = content.slice(range.to_content_range(content.len())?);

        Ok((
            RpRead::new(MetadataBuilder::file(total_size).build()),
            Box::new(content),
        ))
    }
}

fn unsupported() -> Error {
    Error::new(ErrorKind::Unsupported, "operation is not supported")
}

#[derive(Debug, Deserialize)]
struct OciIndex {
    manifests: Vec<Descriptor>,
}

#[derive(Debug, Deserialize)]
struct OciManifest {
    config: Descriptor,
    layers: Vec<Descriptor>,
}

#[derive(Debug, Deserialize)]
struct OciImageConfig {
    architecture: String,
    os: String,
    #[serde(default)]
    variant: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Descriptor {
    #[serde(rename = "mediaType")]
    media_type: String,
    digest: String,
    #[serde(default)]
    annotations: HashMap<String, String>,
    #[serde(default)]
    platform: Option<OciPlatform>,
}

fn load_layout(layout: &Path, reference: &str, platform: &ImagePlatform) -> Result<ContainerCore> {
    let index: OciIndex = read_json(&layout.join("index.json"))?;
    let descriptors = index
        .manifests
        .iter()
        .filter(|descriptor| {
            descriptor
                .annotations
                .get(OCI_REF_NAME)
                .is_some_and(|value| value == reference)
                || descriptor.digest == reference
        })
        .collect::<Vec<_>>();
    if descriptors.is_empty() {
        return Err(Error::new(
            ErrorKind::ConfigInvalid,
            "reference is not found in OCI image layout",
        )
        .with_context("reference", reference.to_string()));
    }
    let manifest = descriptors
        .into_iter()
        .find_map(|descriptor| resolve_manifest(layout, descriptor, platform).transpose())
        .transpose()?
        .ok_or_else(|| {
            Error::new(
                ErrorKind::ConfigInvalid,
                "platform is not found for reference in OCI image layout",
            )
            .with_context("reference", reference.to_string())
            .with_context("platform", platform.to_string())
        })?;
    let mut entries = BTreeMap::new();

    for layer in manifest.layers {
        apply_layer(layout, &layer, &mut entries)?;
    }

    Ok(ContainerCore { entries })
}

fn resolve_manifest(
    layout: &Path,
    descriptor: &Descriptor,
    platform: &ImagePlatform,
) -> Result<Option<OciManifest>> {
    if descriptor
        .platform
        .as_ref()
        .is_some_and(|value| !platform.matches(value))
    {
        return Ok(None);
    }

    match descriptor.media_type.as_str() {
        OCI_IMAGE_MANIFEST | DOCKER_MANIFEST => {
            let manifest: OciManifest = read_json(&blob_path(layout, &descriptor.digest)?)?;
            if descriptor.platform.is_none() {
                let config: OciImageConfig =
                    read_json(&blob_path(layout, &manifest.config.digest)?)?;
                let config_platform = OciPlatform {
                    architecture: config.architecture,
                    os: config.os,
                    variant: config.variant,
                };
                if !platform.matches(&config_platform) {
                    return Ok(None);
                }
            }
            Ok(Some(manifest))
        }
        OCI_IMAGE_INDEX | DOCKER_MANIFEST_LIST => {
            let index: OciIndex = read_json(&blob_path(layout, &descriptor.digest)?)?;
            for descriptor in &index.manifests {
                if let Some(manifest) = resolve_manifest(layout, descriptor, platform)? {
                    return Ok(Some(manifest));
                }
            }
            Ok(None)
        }
        _ => Err(Error::new(
            ErrorKind::Unsupported,
            "unsupported OCI descriptor media type",
        )
        .with_context("media_type", descriptor.media_type.clone())),
    }
}

fn apply_layer(
    layout: &Path,
    descriptor: &Descriptor,
    entries: &mut BTreeMap<String, ContainerEntry>,
) -> Result<()> {
    let path = blob_path(layout, &descriptor.digest)?;
    let file = File::open(&path).map_err(|err| {
        Error::new(ErrorKind::Unexpected, "failed to open OCI layer blob")
            .with_context("path", path.display().to_string())
            .set_source(err)
    })?;

    match descriptor.media_type.as_str() {
        OCI_LAYER_TAR => apply_tar_layer(Archive::new(file), entries),
        OCI_LAYER_TAR_GZIP | DOCKER_LAYER_TAR_GZIP => {
            apply_tar_layer(Archive::new(GzDecoder::new(file)), entries)
        }
        _ => Err(
            Error::new(ErrorKind::Unsupported, "unsupported OCI layer media type")
                .with_context("media_type", descriptor.media_type.clone()),
        ),
    }
}

fn apply_tar_layer<R: Read>(
    mut archive: Archive<R>,
    entries: &mut BTreeMap<String, ContainerEntry>,
) -> Result<()> {
    let archive_entries = archive.entries().map_err(|err| {
        Error::new(
            ErrorKind::Unexpected,
            "failed to read OCI layer tar entries",
        )
        .set_source(err)
    })?;

    let mut whiteouts = Vec::new();
    let mut layer_entries = Vec::new();

    for entry in archive_entries {
        let mut entry = entry.map_err(|err| {
            Error::new(ErrorKind::Unexpected, "failed to read OCI layer tar entry").set_source(err)
        })?;
        let path = entry.path().map_err(|err| {
            Error::new(ErrorKind::Unexpected, "failed to read OCI layer tar path").set_source(err)
        })?;
        let path = normalize_tar_path(path.as_ref());
        if path.is_empty() {
            continue;
        }

        if is_whiteout(&path) {
            whiteouts.push(path);
            continue;
        }

        if entry.header().entry_type().is_dir() {
            let path = ensure_dir_path(&path);
            layer_entries.push((
                path,
                ContainerEntry {
                    metadata: MetadataBuilder::dir().build(),
                    content: None,
                    link: None,
                },
            ));
            continue;
        }

        if entry.header().entry_type().is_file() {
            let mut content = Vec::new();
            entry.read_to_end(&mut content).map_err(|err| {
                Error::new(
                    ErrorKind::Unexpected,
                    "failed to read OCI layer file content",
                )
                .set_source(err)
            })?;
            let content = Bytes::from(content);
            layer_entries.push((
                path,
                ContainerEntry {
                    metadata: MetadataBuilder::file(content.len() as u64).build(),
                    content: Some(content),
                    link: None,
                },
            ));
            continue;
        }

        let link = if entry.header().entry_type().is_symlink() {
            Some(ContainerLink::Symbolic(read_link_name(&entry)?))
        } else if entry.header().entry_type().is_hard_link() {
            Some(ContainerLink::Hard(read_link_name(&entry)?))
        } else {
            None
        };
        layer_entries.push((
            path,
            ContainerEntry {
                metadata: MetadataBuilder::unknown().build(),
                content: None,
                link,
            },
        ));
    }

    for path in whiteouts {
        apply_whiteout(&path, entries)?;
    }
    for (path, entry) in layer_entries {
        replace_entry(entries, path, entry);
    }
    resolve_hardlinks(entries)?;

    Ok(())
}

fn resolve_hardlinks(entries: &mut BTreeMap<String, ContainerEntry>) -> Result<()> {
    let paths = entries
        .iter()
        .filter_map(|(path, entry)| {
            matches!(entry.link, Some(ContainerLink::Hard(_))).then_some(path.clone())
        })
        .collect::<Vec<_>>();
    if paths.is_empty() {
        return Ok(());
    }

    let core = ContainerCore {
        entries: entries.clone(),
    };
    for path in paths {
        let entry = core.get(&path)?.ok_or_else(|| {
            Error::new(
                ErrorKind::Unexpected,
                "OCI layer hard link target is not found",
            )
            .with_context("path", path.clone())
        })?;
        entries.insert(path, entry);
    }
    Ok(())
}

fn is_whiteout(path: &str) -> bool {
    split_parent_name(path).1.starts_with(".wh.")
}

fn apply_whiteout(path: &str, entries: &mut BTreeMap<String, ContainerEntry>) -> Result<()> {
    let (parent, name) = split_parent_name(path);
    if name == ".wh..wh..opq" {
        if parent.is_empty() {
            entries.clear();
        } else {
            remove_prefix(entries, &ensure_dir_path(parent));
        }
        return Ok(());
    }

    if let Some(target) = name.strip_prefix(".wh.") {
        if target.is_empty() {
            return Err(Error::new(
                ErrorKind::Unexpected,
                "OCI layer contains an invalid whiteout",
            ));
        }
        let target = join_path(parent, target);
        entries.remove(&target);
        remove_prefix(entries, &ensure_dir_path(&target));
    }
    Ok(())
}

fn replace_entry(
    entries: &mut BTreeMap<String, ContainerEntry>,
    path: String,
    entry: ContainerEntry,
) {
    let is_dir = path.ends_with('/');
    let plain_path = path.trim_end_matches('/');

    if is_dir {
        entries.remove(plain_path);
    } else {
        entries.remove(&ensure_dir_path(&path));
        remove_prefix(entries, &ensure_dir_path(&path));
    }
    insert_parent_dirs(&path, entries);
    entries.insert(path, entry);
}

fn read_link_name<R: Read>(entry: &tar::Entry<'_, R>) -> Result<String> {
    let target = entry.link_name().map_err(|err| {
        Error::new(
            ErrorKind::Unexpected,
            "failed to read OCI layer link target",
        )
        .set_source(err)
    })?;
    let target = target.ok_or_else(|| {
        Error::new(
            ErrorKind::Unexpected,
            "OCI layer link entry does not have a target",
        )
    })?;
    Ok(target.to_string_lossy().into_owned())
}

fn remove_prefix(entries: &mut BTreeMap<String, ContainerEntry>, prefix: &str) {
    let keys = entries
        .keys()
        .filter(|key| key.starts_with(prefix))
        .cloned()
        .collect::<Vec<_>>();
    for key in keys {
        entries.remove(&key);
    }
}

fn insert_parent_dirs(path: &str, entries: &mut BTreeMap<String, ContainerEntry>) {
    let mut current = String::new();
    let mut parts = path.split('/').peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            break;
        }

        current.push_str(part);
        current.push('/');
        entries.remove(current.trim_end_matches('/'));
        entries
            .entry(current.clone())
            .or_insert_with(|| ContainerEntry {
                metadata: MetadataBuilder::dir().build(),
                content: None,
                link: None,
            });
    }
}

fn split_parent_name(path: &str) -> (&str, &str) {
    match path.rsplit_once('/') {
        Some((parent, name)) => (parent, name),
        None => ("", path),
    }
}

fn join_path(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

fn ensure_dir_path(path: &str) -> String {
    if path.ends_with('/') {
        path.to_string()
    } else {
        format!("{path}/")
    }
}

fn normalize_tar_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let file = File::open(path).map_err(|err| {
        Error::new(ErrorKind::Unexpected, "failed to open OCI json")
            .with_context("path", path.display().to_string())
            .set_source(err)
    })?;

    serde_json::from_reader(file).map_err(|err| {
        Error::new(ErrorKind::Unexpected, "failed to decode OCI json")
            .with_context("path", path.display().to_string())
            .set_source(err)
    })
}

fn blob_path(layout: &Path, digest: &str) -> Result<PathBuf> {
    let (algorithm, encoded) = digest.split_once(':').ok_or_else(|| {
        Error::new(ErrorKind::ConfigInvalid, "invalid OCI descriptor digest")
            .with_context("digest", digest.to_string())
    })?;

    if algorithm != "sha256" {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "unsupported OCI descriptor digest algorithm",
        )
        .with_context("algorithm", algorithm.to_string()));
    }

    Ok(layout.join("blobs").join(algorithm).join(encoded))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;

    use flate2::Compression;
    use flate2::write::GzEncoder;
    use opendal_core::Operator;
    use sha2::Digest;
    use tar::Builder as TarBuilder;
    use tar::Header;

    use super::*;

    #[tokio::test]
    async fn read_and_list_local_oci_layout() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        create_layout(dir.path());

        let op = Operator::new(
            ContainerBuilder::default()
                .layout(dir.path().to_string_lossy())
                .reference("latest")
                .platform(ImagePlatform::linux_amd64()),
        )?;

        let bs = op.read("etc/os-release").await?;
        assert_eq!(bs.to_vec(), b"NAME=OpenDAL\n");

        let entries = op.list("etc/").await?;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path(), "etc/os-release");

        Ok(())
    }

    #[tokio::test]
    async fn whiteout_hides_lower_file() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        create_layout_with_whiteout(dir.path());

        let op = Operator::new(
            ContainerBuilder::default()
                .layout(dir.path().to_string_lossy())
                .reference("latest")
                .platform(ImagePlatform::linux_amd64()),
        )?;

        assert!(op.stat("deleted").await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn whiteouts_do_not_hide_same_layer_entries() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let lower = gzip_layer(&[
            TestEntry::file("foo", b"old"),
            TestEntry::file("dir/old", b"old"),
        ]);
        let upper = gzip_layer(&[
            TestEntry::file("foo", b"new"),
            TestEntry::file(".wh.foo", b""),
            TestEntry::file("dir/new", b"new"),
            TestEntry::file("dir/.wh..wh..opq", b""),
        ]);
        write_layout(dir.path(), vec![lower, upper]);

        let op = operator(dir.path(), "latest", ImagePlatform::linux_amd64())?;
        assert_eq!(op.read("foo").await?.to_vec(), b"new");
        assert_eq!(op.read("dir/new").await?.to_vec(), b"new");
        assert!(op.stat("dir/old").await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn directory_is_replaced_by_file() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let lower = gzip_layer(&[TestEntry::file("foo/bar", b"lower")]);
        let upper = gzip_layer(&[TestEntry::file("foo", b"upper")]);
        write_layout(dir.path(), vec![lower, upper]);

        let op = operator(dir.path(), "latest", ImagePlatform::linux_amd64())?;
        assert_eq!(op.read("foo").await?.to_vec(), b"upper");
        assert!(op.stat("foo/bar").await.is_err());
        assert!(op.list("foo/").await?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn symlink_and_hardlink_are_readable() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let lower = gzip_layer(&[
            TestEntry::file("usr/bin/dash", b"shell"),
            TestEntry::symlink("bin/sh", "../usr/bin/dash"),
            TestEntry::hardlink("usr/bin/dash-copy", "usr/bin/dash"),
        ]);
        let upper = gzip_layer(&[TestEntry::file("usr/bin/dash", b"new-shell")]);
        write_layout(dir.path(), vec![lower, upper]);

        let op = operator(dir.path(), "latest", ImagePlatform::linux_amd64())?;
        assert_eq!(op.read("bin/sh").await?.to_vec(), b"new-shell");
        assert_eq!(op.read("usr/bin/dash-copy").await?.to_vec(), b"shell");
        Ok(())
    }

    #[tokio::test]
    async fn platform_filters_top_level_manifest_descriptors() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        write_multi_platform_layout(dir.path());

        let op = operator(dir.path(), "latest", ImagePlatform::linux_amd64())?;
        assert_eq!(op.read("platform").await?.to_vec(), b"amd64");
        Ok(())
    }

    fn operator(path: &Path, reference: &str, platform: ImagePlatform) -> Result<Operator> {
        Operator::new(
            ContainerBuilder::default()
                .layout(path.to_string_lossy())
                .reference(reference)
                .platform(platform),
        )
    }

    fn create_layout(path: &Path) {
        let layer = gzip_layer(&[TestEntry::file("etc/os-release", b"NAME=OpenDAL\n")]);
        write_layout(path, vec![layer]);
    }

    fn create_layout_with_whiteout(path: &Path) {
        let lower = gzip_layer(&[TestEntry::file("deleted", b"deleted")]);
        let upper = gzip_layer(&[TestEntry::file(".wh.deleted", b"")]);
        write_layout(path, vec![lower, upper]);
    }

    fn write_layout(path: &Path, layers: Vec<Vec<u8>>) {
        fs::create_dir_all(path.join("blobs/sha256")).unwrap();
        fs::write(path.join("oci-layout"), r#"{"imageLayoutVersion":"1.0.0"}"#).unwrap();

        let mut layer_descriptors = Vec::new();
        for layer in layers {
            let digest = write_blob(path, &layer);
            layer_descriptors.push(serde_json::json!({
                "mediaType": OCI_LAYER_TAR_GZIP,
                "digest": digest,
                "size": layer.len(),
            }));
        }

        let config = serde_json::json!({
            "architecture": "amd64",
            "os": "linux",
            "rootfs": {"type": "layers", "diff_ids": []},
        })
        .to_string();
        let config_digest = write_blob(path, config.as_bytes());
        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": OCI_IMAGE_MANIFEST,
            "config": {
                "mediaType": "application/vnd.oci.image.config.v1+json",
                "digest": config_digest,
                "size": config.len(),
            },
            "layers": layer_descriptors,
        })
        .to_string();
        let manifest_digest = write_blob(path, manifest.as_bytes());
        let index = serde_json::json!({
            "schemaVersion": 2,
            "manifests": [{
                "mediaType": OCI_IMAGE_MANIFEST,
                "digest": manifest_digest,
                "size": manifest.len(),
                "annotations": { (OCI_REF_NAME): "latest" },
                "platform": {"architecture": "amd64", "os": "linux"},
            }],
        })
        .to_string();

        fs::write(path.join("index.json"), index).unwrap();
    }

    fn write_multi_platform_layout(path: &Path) {
        fs::create_dir_all(path.join("blobs/sha256")).unwrap();
        fs::write(path.join("oci-layout"), r#"{"imageLayoutVersion":"1.0.0"}"#).unwrap();

        let manifests = [("arm64", b"arm64".as_slice()), ("amd64", b"amd64".as_slice())]
            .map(|(architecture, content)| {
                let layer = gzip_layer(&[TestEntry::file("platform", content)]);
                let layer_digest = write_blob(path, &layer);
                let config = serde_json::json!({"architecture": architecture, "os": "linux"})
                    .to_string();
                let config_digest = write_blob(path, config.as_bytes());
                let manifest = serde_json::json!({
                    "schemaVersion": 2,
                    "mediaType": OCI_IMAGE_MANIFEST,
                    "config": {"mediaType": "application/vnd.oci.image.config.v1+json", "digest": config_digest, "size": config.len()},
                    "layers": [{"mediaType": OCI_LAYER_TAR_GZIP, "digest": layer_digest, "size": layer.len()}],
                })
                .to_string();
                let digest = write_blob(path, manifest.as_bytes());
                serde_json::json!({
                    "mediaType": OCI_IMAGE_MANIFEST,
                    "digest": digest,
                    "size": manifest.len(),
                    "annotations": {(OCI_REF_NAME): "latest"},
                    "platform": {"architecture": architecture, "os": "linux"},
                })
            });

        fs::write(
            path.join("index.json"),
            serde_json::json!({"schemaVersion": 2, "manifests": manifests}).to_string(),
        )
        .unwrap();
    }

    enum TestEntry<'a> {
        File(&'a str, &'a [u8]),
        Symlink(&'a str, &'a str),
        Hardlink(&'a str, &'a str),
    }

    impl<'a> TestEntry<'a> {
        fn file(path: &'a str, content: &'a [u8]) -> Self {
            Self::File(path, content)
        }

        fn symlink(path: &'a str, target: &'a str) -> Self {
            Self::Symlink(path, target)
        }

        fn hardlink(path: &'a str, target: &'a str) -> Self {
            Self::Hardlink(path, target)
        }
    }

    fn gzip_layer(entries: &[TestEntry<'_>]) -> Vec<u8> {
        let mut tar = Vec::new();
        {
            let mut builder = TarBuilder::new(&mut tar);
            for entry in entries {
                let mut header = Header::new_gnu();
                let (path, content) = match entry {
                    TestEntry::File(path, content) => (*path, *content),
                    TestEntry::Symlink(path, target) => {
                        header.set_entry_type(tar::EntryType::Symlink);
                        header.set_link_name(target).unwrap();
                        (*path, b"".as_slice())
                    }
                    TestEntry::Hardlink(path, target) => {
                        header.set_entry_type(tar::EntryType::Link);
                        header.set_link_name(target).unwrap();
                        (*path, b"".as_slice())
                    }
                };
                header.set_path(path).unwrap();
                header.set_size(content.len() as u64);
                header.set_cksum();
                builder.append(&header, content).unwrap();
            }
            builder.finish().unwrap();
        }

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&tar).unwrap();
        encoder.finish().unwrap()
    }

    fn write_blob(path: &Path, content: &[u8]) -> String {
        let digest = sha2::Sha256::digest(content);
        let encoded = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        fs::write(path.join("blobs/sha256").join(&encoded), content).unwrap();
        format!("sha256:{encoded}")
    }
}
