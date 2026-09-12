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

use std::vec::IntoIter;

use opendal_core::raw::*;
use opendal_core::*;

pub(crate) struct ContainerLister {
    root: String,
    iter: IntoIter<(String, Metadata)>,
}

impl ContainerLister {
    pub(crate) fn new(root: String, entries: Vec<(String, Metadata)>) -> Self {
        Self {
            root,
            iter: entries.into_iter(),
        }
    }
}

impl oio::List for ContainerLister {
    async fn next(&mut self) -> Result<Option<oio::Entry>> {
        for (key, metadata) in self.iter.by_ref() {
            if key == self.root[1..] {
                continue;
            }

            let path = build_rel_path(&self.root, &key);
            return Ok(Some(oio::Entry::new(&path, metadata)));
        }

        Ok(None)
    }
}
