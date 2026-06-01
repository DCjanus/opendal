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
pub(crate) struct ContainerEntry {
    pub(crate) metadata: Metadata,
    pub(crate) content: Option<Bytes>,
}

#[derive(Debug)]
pub(crate) struct ContainerCore {
    pub(crate) entries: BTreeMap<String, ContainerEntry>,
}

impl ContainerCore {
    pub(crate) fn get(&self, path: &str) -> Option<ContainerEntry> {
        self.entries.get(path).cloned()
    }

    pub(crate) fn has_children(&self, path: &str) -> bool {
        self.entries.keys().any(|key| key.starts_with(path))
    }

    pub(crate) fn scan(&self, path: &str) -> Vec<(String, Metadata)> {
        self.entries
            .iter()
            .filter(|(key, _)| key.starts_with(path) && key.as_str() != path)
            .map(|(key, value)| (key.clone(), value.metadata.clone()))
            .collect()
    }
}
