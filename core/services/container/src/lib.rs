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

#![cfg_attr(docsrs, feature(doc_cfg))]
//! Container image service implementation for Apache OpenDAL.
//!
//! This service is experimental and currently supports local OCI image layouts.
#![deny(missing_docs)]

mod backend;
mod config;
mod core;
mod lister;

pub use backend::ContainerBuilder as Container;
pub use config::ContainerConfig;
pub use config::ImagePlatform;

/// Default scheme for container service.
pub const CONTAINER_SCHEME: &str = "container";

/// Register this service into the given registry.
pub fn register_container_service(registry: &opendal_core::OperatorRegistry) {
    registry.register::<Container>(CONTAINER_SCHEME);
}
