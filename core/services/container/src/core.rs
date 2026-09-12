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

use bytes::Bytes;
use opendal_core::*;

#[derive(Debug, Clone)]
pub(crate) enum ContainerLink {
    Symbolic(String),
    Hard(String),
}

#[derive(Debug, Clone)]
pub(crate) struct ContainerEntry {
    pub(crate) metadata: Metadata,
    pub(crate) content: Option<Bytes>,
    pub(crate) link: Option<ContainerLink>,
}

#[derive(Debug)]
pub(crate) struct ContainerCore {
    pub(crate) entries: BTreeMap<String, ContainerEntry>,
}

impl ContainerCore {
    pub(crate) fn has_children(&self, path: &str) -> bool {
        self.entries.keys().any(|key| key.starts_with(path))
    }

    pub(crate) fn scan(&self, path: &str) -> Vec<(String, Metadata)> {
        self.entries
            .iter()
            .filter(|(key, _)| key.starts_with(path))
            .map(|(key, value)| (key.clone(), value.metadata.clone()))
            .collect()
    }

    pub(crate) fn resolve_path(&self, path: &str) -> Result<Option<String>> {
        let mut path = path.trim_start_matches('/').to_string();
        for _ in 0..=self.entries.len() {
            let components = path
                .split('/')
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>();
            let mut prefix = String::new();
            let mut followed = false;

            for (index, component) in components.iter().enumerate() {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(component);

                let Some(link) = self
                    .entries
                    .get(&prefix)
                    .and_then(|entry| entry.link.as_ref())
                else {
                    continue;
                };

                let target = match link {
                    ContainerLink::Symbolic(target) => {
                        let parent = prefix.rsplit_once('/').map_or("", |(parent, _)| parent);
                        normalize_link_target(parent, target)?
                    }
                    ContainerLink::Hard(target) => normalize_link_target("", target)?,
                };
                let remaining = components[index + 1..].join("/");
                path = if remaining.is_empty() {
                    target
                } else if target.is_empty() {
                    remaining
                } else {
                    format!("{target}/{remaining}")
                };
                followed = true;
                break;
            }

            if !followed {
                if self.entries.contains_key(&path) {
                    return Ok(Some(path));
                }
                let directory = format!("{}/", path.trim_end_matches('/'));
                return Ok(self.entries.contains_key(&directory).then_some(directory));
            }
        }

        Err(Error::new(
            ErrorKind::Unexpected,
            "container image contains a link cycle",
        ))
    }
}

pub(crate) fn normalize_link_target(parent: &str, target: &str) -> Result<String> {
    let mut components = if target.starts_with('/') || parent.is_empty() {
        Vec::new()
    } else {
        parent.split('/').map(str::to_string).collect()
    };

    for component in std::path::Path::new(target).components() {
        match component {
            std::path::Component::RootDir | std::path::Component::CurDir => {}
            std::path::Component::Normal(part) => {
                components.push(part.to_string_lossy().into_owned())
            }
            std::path::Component::ParentDir => {
                if components.pop().is_none() {
                    return Err(Error::new(
                        ErrorKind::PermissionDenied,
                        "container image link escapes the image rootfs",
                    ));
                }
            }
            std::path::Component::Prefix(_) => {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    "container image link uses an unsupported path prefix",
                ));
            }
        }
    }

    Ok(components.join("/"))
}
